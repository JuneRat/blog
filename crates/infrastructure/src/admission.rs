//! Bounded single-process token buckets for public work. No active bucket is
//! evicted to admit a new source, and denied requests do not extend lockouts.
use application::{
    UseCaseError,
    ports::{PublicRequest, RequestAdmission},
};
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

const WINDOW: Duration = Duration::from_secs(60);
const MAX_CLIENTS: usize = 2048;

struct Bucket {
    tokens: f64,
    updated: Instant,
}
impl Bucket {
    fn new(capacity: u32, now: Instant) -> Self {
        Self {
            tokens: capacity as f64,
            updated: now,
        }
    }
    fn refresh(&mut self, capacity: u32, now: Instant) {
        self.tokens = (self.tokens
            + now.saturating_duration_since(self.updated).as_secs_f64() * capacity as f64
                / WINDOW.as_secs_f64())
        .min(capacity as f64);
        self.updated = now;
    }
    fn wait(&self, capacity: u32) -> u64 {
        if self.tokens >= 1.0 {
            0
        } else {
            ((1.0 - self.tokens) * WINDOW.as_secs_f64() / capacity as f64)
                .ceil()
                .max(1.0) as u64
        }
    }
}
struct Budget {
    global: Bucket,
    clients: HashMap<String, Bucket>,
    swept: Instant,
}
pub struct InMemoryRequestAdmission {
    budgets: Mutex<HashMap<PublicRequest, Budget>>,
    clock: Box<dyn Fn() -> Instant + Send + Sync>,
}
impl Default for InMemoryRequestAdmission {
    fn default() -> Self {
        Self::new(Box::new(Instant::now))
    }
}
impl InMemoryRequestAdmission {
    fn new(clock: Box<dyn Fn() -> Instant + Send + Sync>) -> Self {
        Self {
            budgets: Mutex::new(HashMap::new()),
            clock,
        }
    }
}
impl RequestAdmission for InMemoryRequestAdmission {
    fn admit(&self, action: PublicRequest, client: Option<&str>) -> Result<(), UseCaseError> {
        let (client_limit, global_limit) = match action {
            // At most 660 admitted starts in any 600 s: below the 1000-state pool.
            PublicRequest::Registration => (3, 30),
            PublicRequest::OAuthStart => (10, 60),
            PublicRequest::CommentSubmit => (5, 120),
            PublicRequest::CommentPreview => (20, 240),
        };
        let now = (self.clock)();
        let mut budgets = self
            .budgets
            .lock()
            .map_err(|_| UseCaseError::Repository("请求准入锁损坏".into()))?;
        let budget = budgets.entry(action).or_insert_with(|| Budget {
            global: Bucket::new(global_limit, now),
            clients: HashMap::new(),
            swept: now,
        });
        if now.saturating_duration_since(budget.swept) >= WINDOW {
            budget
                .clients
                .retain(|_, bucket| now.saturating_duration_since(bucket.updated) < WINDOW);
            budget.swept = now;
        }
        budget.global.refresh(global_limit, now);
        let key = client.unwrap_or("unknown");
        let global_wait = budget.global.wait(global_limit);
        let client_wait = budget
            .clients
            .get_mut(key)
            .map(|bucket| {
                bucket.refresh(client_limit, now);
                bucket.wait(client_limit)
            })
            .unwrap_or(0);
        let wait = global_wait.max(client_wait);
        if wait > 0 {
            return Err(UseCaseError::RateLimited {
                retry_after_secs: wait,
            });
        }
        if !budget.clients.contains_key(key) && budget.clients.len() >= MAX_CLIENTS {
            return Err(UseCaseError::RateLimited {
                retry_after_secs: WINDOW.as_secs(),
            });
        }
        let bucket = budget
            .clients
            .entry(key.into())
            .or_insert_with(|| Bucket::new(client_limit, now));
        bucket.tokens -= 1.0;
        budget.global.tokens -= 1.0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn client_global_and_action_budgets_are_independent_and_recover() {
        let now = Arc::new(Mutex::new(Instant::now()));
        let clock = now.clone();
        let limits = InMemoryRequestAdmission::new(Box::new(move || *clock.lock().unwrap()));
        for _ in 0..10 {
            limits.admit(PublicRequest::OAuthStart, Some("a")).unwrap();
        }
        assert!(matches!(
            limits.admit(PublicRequest::OAuthStart, Some("a")),
            Err(UseCaseError::RateLimited {
                retry_after_secs: 6
            })
        ));
        limits
            .admit(PublicRequest::CommentSubmit, Some("a"))
            .unwrap();
        for i in 0..50 {
            limits
                .admit(PublicRequest::OAuthStart, Some(&i.to_string()))
                .unwrap();
        }
        assert!(matches!(
            limits.admit(PublicRequest::OAuthStart, Some("new")),
            Err(UseCaseError::RateLimited { .. })
        ));
        *now.lock().unwrap() += Duration::from_secs(6);
        limits.admit(PublicRequest::OAuthStart, Some("a")).unwrap();
        *now.lock().unwrap() += WINDOW;
        limits
            .admit(PublicRequest::OAuthStart, Some("new"))
            .unwrap();
        assert_eq!(
            limits.budgets.lock().unwrap()[&PublicRequest::OAuthStart]
                .clients
                .len(),
            1
        );
    }
    #[test]
    fn parallel_admission_cannot_overbook_a_client() {
        let limits = InMemoryRequestAdmission::default();
        let accepted = std::thread::scope(|s| {
            let handles: Vec<_> = (0..50)
                .map(|_| s.spawn(|| limits.admit(PublicRequest::CommentSubmit, None).is_ok()))
                .collect();
            handles
                .into_iter()
                .map(|h| usize::from(h.join().unwrap()))
                .sum::<usize>()
        });
        assert_eq!(accepted, 5);
    }
}
