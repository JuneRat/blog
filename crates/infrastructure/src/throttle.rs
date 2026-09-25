//! 单实例内存登录限流：并发预占 + 失败计数 + 临时锁定。
//!
//! 与内存会话一样，本版只支持单实例；重启会清空计数（不会清空凭据）。
//!
//! 采用**预占**模型（见 [`LoginThrottle`]）：昂贵的口令校验之前先占额度，
//! 出结果后转为失败或成功。只在事后计数会留下 TOCTOU 窗口——N 个并发请求
//! 会在任何一次失败被记录之前全部通过检查，阈值形同虚设。
//!
//! 其它语义：
//! - 账号维度阈值低（默认 5 次 / 15 分钟），来源地址维度阈值高（默认 50 次），
//!   这样即使部署在反向代理后所有请求共用一个来源地址，也不会被单个用户名拖垮。
//! - 锁定期间 [`reserve`] 拒绝且**不延长**锁定，避免攻击者用已知用户名把真实用户
//!   永久锁在门外。
//! - 锁定到期后给一个干净的窗口重新尝试，不做逐次指数退避（行为更可预期）。

use std::collections::HashMap;
use std::sync::Mutex;

use application::error::UseCaseError;
use application::ports::{LoginThrottle, ThrottleDecision, ThrottleSubject};
use time::OffsetDateTime;

/// 限流参数。
pub struct ThrottleConfig {
    /// 账号维度：窗口内允许的尝试次数（失败与在飞共享该上限）。
    pub user_max_failures: u32,
    /// 账号维度：达到阈值后的锁定时长（秒）。
    pub user_lock_secs: u64,
    /// 客户端地址维度：窗口内允许的尝试次数。
    pub client_max_failures: u32,
    /// 客户端地址维度：达到阈值后的锁定时长（秒）。
    pub client_lock_secs: u64,
    /// 失败计数窗口（秒）：窗口内无新失败则计数归零。
    pub window_secs: i64,
    /// 计数条目上限；淘汰最早的空闲主体，无空闲条目时拒绝新主体。
    pub max_entries: usize,
}

impl Default for ThrottleConfig {
    fn default() -> Self {
        Self {
            user_max_failures: 5,
            user_lock_secs: 15 * 60,
            client_max_failures: 50,
            client_lock_secs: 15 * 60,
            window_secs: 15 * 60,
            max_entries: 10_000,
        }
    }
}

/// 单个限流主体的状态。
struct FailureState {
    /// 窗口内已确认的失败次数。
    failures: u32,
    /// 已预占但尚未出结果的尝试数；与 `failures` 共享阈值。
    in_flight: u32,
    /// 当前计数窗口的起点。
    window_started_at: OffsetDateTime,
    /// 锁定截止时间；`None` 表示未锁定。
    locked_until: Option<OffsetDateTime>,
}

impl FailureState {
    fn new(now: OffsetDateTime) -> Self {
        Self {
            failures: 0,
            in_flight: 0,
            window_started_at: now,
            locked_until: None,
        }
    }
}

pub struct InMemoryLoginThrottle {
    config: ThrottleConfig,
    clock: Box<dyn Fn() -> OffsetDateTime + Send + Sync>,
    state: Mutex<HashMap<ThrottleSubject, FailureState>>,
}

impl InMemoryLoginThrottle {
    /// 取共享状态；锁中毒时恢复数据继续使用。
    ///
    /// 中毒只说明某线程持锁时 panic——限流状态是启发式计数而非不变量数据，
    /// 最坏情况是某个计数偏差一位，随窗口滚动自愈。这里**不**向上传播 panic：
    /// 登录限流路径上崩溃会把所有后来登录一起拖死，比计数偏差严重得多。
    fn locked_state(&self) -> std::sync::MutexGuard<'_, HashMap<ThrottleSubject, FailureState>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn new(
        config: ThrottleConfig,
        clock: Box<dyn Fn() -> OffsetDateTime + Send + Sync>,
    ) -> Self {
        Self {
            config,
            clock,
            state: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(ThrottleConfig::default(), Box::new(OffsetDateTime::now_utc))
    }

    fn now(&self) -> OffsetDateTime {
        (self.clock)()
    }

    fn window(&self) -> time::Duration {
        time::Duration::seconds(self.config.window_secs)
    }

    fn policy(&self, subject: &ThrottleSubject) -> (u32, u64) {
        match subject {
            ThrottleSubject::User(_) => (self.config.user_max_failures, self.config.user_lock_secs),
            ThrottleSubject::Client(_) => (
                self.config.client_max_failures,
                self.config.client_lock_secs,
            ),
        }
    }

    /// 清理已过期条目。调用方必须已持有 `state` 锁。
    ///
    /// 只清理「过期」，不做容量淘汰：淘汰只在真的要插入新主体时进行，
    /// 否则每次操作都 prune 会在两个已存在的主体之间反复挤掉对方。
    fn prune_expired(
        &self,
        state: &mut HashMap<ThrottleSubject, FailureState>,
        now: OffsetDateTime,
    ) {
        let window = self.window();
        state.retain(|_, entry| {
            let locked = entry.locked_until.is_some_and(|until| until > now);
            // 有在飞请求的条目不能丢：否则归还预占会落空。
            locked || entry.in_flight > 0 || now - entry.window_started_at <= window
        });
    }

    /// 为新主体腾容量：淘汰窗口起点最早、且没有在飞请求的条目。内存因此有界。
    fn ensure_capacity(&self, state: &mut HashMap<ThrottleSubject, FailureState>) {
        if state.len() < self.config.max_entries {
            return;
        }
        let mut oldest: Vec<(ThrottleSubject, OffsetDateTime)> = state
            .iter()
            .filter(|(_, entry)| entry.in_flight == 0)
            .map(|(subject, entry)| (subject.clone(), entry.window_started_at))
            .collect();
        oldest.sort_by_key(|(_, started)| *started);
        let overflow = state.len() + 1 - self.config.max_entries;
        for (subject, _) in oldest.into_iter().take(overflow) {
            state.remove(&subject);
        }
    }

    /// 取得（必要时创建）主体状态；创建前先腾容量。调用方必须已持有 `state` 锁。
    fn entry_mut<'a>(
        &self,
        state: &'a mut HashMap<ThrottleSubject, FailureState>,
        subject: &ThrottleSubject,
        now: OffsetDateTime,
    ) -> Option<&'a mut FailureState> {
        if !state.contains_key(subject) {
            self.ensure_capacity(state);
            if state.len() >= self.config.max_entries {
                return None;
            }
        }
        Some(
            state
                .entry(subject.clone())
                .or_insert_with(|| FailureState::new(now)),
        )
    }

    /// 归一化窗口状态：锁定到期或窗口过期都重置计数；返回时条目未处于活动锁定。
    fn refresh_window(&self, entry: &mut FailureState, now: OffsetDateTime) {
        if let Some(until) = entry.locked_until {
            if until > now {
                return;
            }
            // 锁定到期：给用户一个干净的计数窗口，而不是带着旧计数立即再次锁定。
            entry.locked_until = None;
            entry.failures = 0;
            entry.window_started_at = now;
            return;
        }
        if now - entry.window_started_at > self.window() {
            entry.failures = 0;
            entry.window_started_at = now;
        }
    }
}

impl LoginThrottle for InMemoryLoginThrottle {
    fn reserve(&self, subject: &ThrottleSubject) -> Result<ThrottleDecision, UseCaseError> {
        let now = self.now();
        let (max_attempts, _) = self.policy(subject);
        let mut state = self.locked_state();
        self.prune_expired(&mut state, now);
        let Some(entry) = self.entry_mut(&mut state, subject, now) else {
            return Ok(ThrottleDecision {
                allowed: false,
                retry_after_secs: 1,
            });
        };
        self.refresh_window(entry, now);

        if let Some(until) = entry.locked_until {
            return Ok(ThrottleDecision {
                allowed: false,
                retry_after_secs: (until - now).whole_seconds().max(1) as u64,
            });
        }
        // 失败与在飞共享额度：并发请求同样只能放行阈值内的次数。
        if entry.failures + entry.in_flight >= max_attempts {
            // 额度被在飞请求占满但尚未锁定：在飞的很快出结果，短等待即可。
            return Ok(ThrottleDecision {
                allowed: false,
                retry_after_secs: 1,
            });
        }
        entry.in_flight += 1;
        Ok(ThrottleDecision {
            allowed: true,
            retry_after_secs: 0,
        })
    }

    fn release(&self, subject: &ThrottleSubject) -> Result<(), UseCaseError> {
        let now = self.now();
        let mut state = self.locked_state();
        self.prune_expired(&mut state, now);
        if let Some(entry) = state.get_mut(subject) {
            entry.in_flight = entry.in_flight.saturating_sub(1);
        }
        Ok(())
    }

    fn record_failure(&self, subject: &ThrottleSubject) -> Result<(), UseCaseError> {
        let now = self.now();
        let (max_attempts, lock_secs) = self.policy(subject);
        let mut state = self.locked_state();
        self.prune_expired(&mut state, now);
        // 落账只处理实际预占，不能绕过容量上限创建新主体。
        let Some(entry) = state.get_mut(subject) else {
            return Ok(());
        };
        self.refresh_window(entry, now);
        entry.in_flight = entry.in_flight.saturating_sub(1);
        entry.failures = entry.failures.saturating_add(1);
        // 已在锁定中不延长（正常路径下 reserve 已拒绝，这里是防御性判断）。
        if entry.locked_until.is_none() && entry.failures >= max_attempts {
            entry.locked_until = Some(now + time::Duration::seconds(lock_secs as i64));
        }
        Ok(())
    }

    fn record_success(&self, subject: &ThrottleSubject) -> Result<(), UseCaseError> {
        let now = self.now();
        let mut state = self.locked_state();
        self.prune_expired(&mut state, now);
        match subject {
            // 成功只结算本次请求，不能抹掉其他请求持有的预占。
            ThrottleSubject::User(_) => {
                if let Some(entry) = state.get_mut(subject) {
                    entry.in_flight = entry.in_flight.saturating_sub(1);
                    entry.failures = 0;
                    entry.locked_until = None;
                    entry.window_started_at = now;
                }
            }
            // 来源地址维度：只归还本次预占，保留历史失败——
            // 否则攻击者可用自有账号反复成功登录来清零该维度。
            ThrottleSubject::Client(_) => {
                if let Some(entry) = state.get_mut(subject) {
                    entry.in_flight = entry.in_flight.saturating_sub(1);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn mutable_now() -> (
        Arc<Mutex<OffsetDateTime>>,
        Box<dyn Fn() -> OffsetDateTime + Send + Sync>,
    ) {
        let now = Arc::new(Mutex::new(OffsetDateTime::now_utc()));
        let reader = now.clone();
        (now, Box::new(move || *reader.lock().unwrap()))
    }

    fn throttle(config: ThrottleConfig) -> (InMemoryLoginThrottle, Arc<Mutex<OffsetDateTime>>) {
        let (now, clock) = mutable_now();
        (InMemoryLoginThrottle::new(config, clock), now)
    }

    fn failures_of(throttle: &InMemoryLoginThrottle, subject: &ThrottleSubject) -> u32 {
        throttle
            .state
            .lock()
            .unwrap()
            .get(subject)
            .map(|entry| entry.failures)
            .unwrap_or(0)
    }

    fn in_flight_of(throttle: &InMemoryLoginThrottle, subject: &ThrottleSubject) -> u32 {
        throttle
            .state
            .lock()
            .unwrap()
            .get(subject)
            .map(|entry| entry.in_flight)
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn account_locks_after_threshold_and_does_not_extend() {
        let config = ThrottleConfig {
            user_max_failures: 3,
            user_lock_secs: 60,
            ..ThrottleConfig::default()
        };
        let (throttle, now) = throttle(config);
        let subject = ThrottleSubject::User("sun".into());

        for _ in 0..3 {
            assert!(throttle.reserve(&subject).unwrap().allowed);
            throttle.record_failure(&subject).unwrap();
        }
        let locked = throttle.reserve(&subject).unwrap();
        assert!(!locked.allowed);
        assert!(locked.retry_after_secs > 0);

        // 锁定期间的重复预占不得延长锁定。
        *now.lock().unwrap() += time::Duration::seconds(30);
        let still = throttle.reserve(&subject).unwrap();
        assert!(!still.allowed);
        assert!(still.retry_after_secs <= 30, "{still:?}");

        // 锁定期满后恢复，并给一个干净的计数窗口。
        *now.lock().unwrap() += time::Duration::seconds(31);
        assert!(throttle.reserve(&subject).unwrap().allowed);
        throttle.release(&subject).unwrap();
        assert_eq!(failures_of(&throttle, &subject), 0);
    }

    /// 并发请求不得超发额度：预占计入上限，在飞期间第 N+1 个就被拒。
    #[tokio::test]
    async fn concurrent_reservations_cannot_exceed_the_threshold() {
        let (throttle, _) = throttle(ThrottleConfig {
            user_max_failures: 3,
            ..ThrottleConfig::default()
        });
        let subject = ThrottleSubject::User("sun".into());

        for _ in 0..3 {
            assert!(throttle.reserve(&subject).unwrap().allowed);
        }
        let denied = throttle.reserve(&subject).unwrap();
        assert!(!denied.allowed, "并发预占不得超发额度");
        assert_eq!(in_flight_of(&throttle, &subject), 3);

        // 归还一个额度后，下一个才能进来。
        throttle.release(&subject).unwrap();
        assert!(throttle.reserve(&subject).unwrap().allowed);
    }

    #[tokio::test]
    async fn client_and_user_dimensions_count_independently() {
        let config = ThrottleConfig {
            user_max_failures: 1,
            user_lock_secs: 60,
            client_max_failures: 3,
            client_lock_secs: 60,
            ..ThrottleConfig::default()
        };
        let (throttle, _) = throttle(config);
        let user = ThrottleSubject::User("a".into());
        let client = ThrottleSubject::Client("203.0.113.7".into());

        assert!(throttle.reserve(&user).unwrap().allowed);
        throttle.record_failure(&user).unwrap();
        assert!(!throttle.reserve(&user).unwrap().allowed);
        // 客户端维度仍有额度。
        assert!(throttle.reserve(&client).unwrap().allowed);

        throttle.record_failure(&client).unwrap();
        throttle.reserve(&client).unwrap();
        throttle.record_failure(&client).unwrap();
        throttle.reserve(&client).unwrap();
        throttle.record_failure(&client).unwrap();
        assert!(!throttle.reserve(&client).unwrap().allowed);
    }

    #[test]
    fn success_preserves_other_reservations() {
        let (throttle, _) = throttle(ThrottleConfig {
            user_max_failures: 3,
            ..ThrottleConfig::default()
        });
        let user = ThrottleSubject::User("sun".into());
        for _ in 0..3 {
            assert!(throttle.reserve(&user).unwrap().allowed);
        }
        throttle.record_success(&user).unwrap();
        assert_eq!(in_flight_of(&throttle, &user), 2);
        assert!(throttle.reserve(&user).unwrap().allowed);
        assert!(!throttle.reserve(&user).unwrap().allowed);
        throttle.record_failure(&user).unwrap();
        throttle.release(&user).unwrap();
        assert_eq!(in_flight_of(&throttle, &user), 1);
        assert_eq!(failures_of(&throttle, &user), 1);
    }

    #[test]
    fn capacity_is_bounded_when_all_entries_are_in_flight() {
        let (throttle, now) = throttle(ThrottleConfig {
            max_entries: 2,
            ..ThrottleConfig::default()
        });
        let a = ThrottleSubject::User("a".into());
        let b = ThrottleSubject::User("b".into());
        let c = ThrottleSubject::User("c".into());
        assert!(throttle.reserve(&a).unwrap().allowed);
        assert!(throttle.reserve(&b).unwrap().allowed);
        *now.lock().unwrap() += time::Duration::days(1);
        let denied = throttle.reserve(&c).unwrap();
        assert!(!denied.allowed);
        assert!(denied.retry_after_secs >= 1);
        assert_eq!(throttle.state.lock().unwrap().len(), 2);
        // 已有主体仍可使用自己的剩余额度；未预占的落账不能插入条目。
        assert!(throttle.reserve(&a).unwrap().allowed);
        throttle.record_failure(&c).unwrap();
        assert_eq!(throttle.state.lock().unwrap().len(), 2);
        throttle.release(&b).unwrap();
        assert!(throttle.reserve(&c).unwrap().allowed);
        assert_eq!(throttle.state.lock().unwrap().len(), 2);
        assert_eq!(in_flight_of(&throttle, &a), 2);
    }

    #[test]
    fn zero_capacity_rejects_without_inserting() {
        let (throttle, _) = throttle(ThrottleConfig {
            max_entries: 0,
            ..ThrottleConfig::default()
        });
        let subject = ThrottleSubject::User("sun".into());
        assert!(!throttle.reserve(&subject).unwrap().allowed);
        assert!(throttle.state.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn success_clears_account_history_but_not_client_history() {
        let (throttle, _) = throttle(ThrottleConfig::default());
        let user = ThrottleSubject::User("sun".into());
        let client = ThrottleSubject::Client("198.51.100.9".into());

        for subject in [&user, &client] {
            assert!(throttle.reserve(subject).unwrap().allowed);
            throttle.record_failure(subject).unwrap();
        }
        assert_eq!(failures_of(&throttle, &user), 1);
        assert_eq!(failures_of(&throttle, &client), 1);

        for subject in [&user, &client] {
            assert!(throttle.reserve(subject).unwrap().allowed);
            throttle.record_success(subject).unwrap();
        }
        assert_eq!(failures_of(&throttle, &user), 0, "账号维度成功即清空");
        assert_eq!(
            failures_of(&throttle, &client),
            1,
            "来源地址维度必须保留历史失败"
        );
        assert_eq!(in_flight_of(&throttle, &client), 0, "预占已归还");
    }

    /// 归还预占不等于失败：不计入失败次数，也不会推进锁定。
    #[tokio::test]
    async fn release_returns_capacity_without_counting_a_failure() {
        let (throttle, _) = throttle(ThrottleConfig {
            user_max_failures: 1,
            ..ThrottleConfig::default()
        });
        let subject = ThrottleSubject::User("sun".into());

        assert!(throttle.reserve(&subject).unwrap().allowed);
        throttle.release(&subject).unwrap();
        assert_eq!(failures_of(&throttle, &subject), 0);
        assert_eq!(in_flight_of(&throttle, &subject), 0);
        // 额度已恢复，仍可再预占。
        assert!(throttle.reserve(&subject).unwrap().allowed);
    }

    #[tokio::test]
    async fn stale_entries_are_pruned() {
        let (throttle, now) = throttle(ThrottleConfig {
            window_secs: 60,
            ..ThrottleConfig::default()
        });
        let subject = ThrottleSubject::User("sun".into());
        assert!(throttle.reserve(&subject).unwrap().allowed);
        throttle.record_failure(&subject).unwrap();

        *now.lock().unwrap() += time::Duration::seconds(61);
        assert!(throttle.reserve(&subject).unwrap().allowed);
        throttle.release(&subject).unwrap();
        assert_eq!(failures_of(&throttle, &subject), 0, "窗口过期后计数归零");
    }

    /// 容量淘汰：超出 max_entries 时丢弃窗口起点最早的主体（而不是拒绝新主体）。
    #[tokio::test]
    async fn capacity_eviction_drops_the_oldest_subject() {
        let (throttle, now) = throttle(ThrottleConfig {
            user_max_failures: 10,
            window_secs: 10_000,
            max_entries: 2,
            ..ThrottleConfig::default()
        });
        let a = ThrottleSubject::User("a".into());
        let b = ThrottleSubject::User("b".into());
        let c = ThrottleSubject::User("c".into());

        throttle.reserve(&a).unwrap();
        throttle.record_failure(&a).unwrap();
        *now.lock().unwrap() += time::Duration::seconds(1);
        throttle.reserve(&b).unwrap();
        throttle.record_failure(&b).unwrap();
        *now.lock().unwrap() += time::Duration::seconds(1);
        // 第三个主体触发淘汰：a 的窗口起点最早，先被丢弃。
        throttle.reserve(&c).unwrap();
        throttle.record_failure(&c).unwrap();

        assert_eq!(failures_of(&throttle, &a), 0, "被淘汰的主体的计数归零");
        assert_eq!(failures_of(&throttle, &b), 1);
        assert_eq!(failures_of(&throttle, &c), 1);
        assert_eq!(throttle.state.lock().unwrap().len(), 2, "条目数不超过上限");
    }
}
