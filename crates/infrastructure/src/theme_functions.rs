//! Per-render public theme functions. Captured state is never shared between requests.
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use application::error::UseCaseError;
use application::theme_data::ThemeData;
use minijinja::value::Kwargs;
use minijinja::{Environment, Error, ErrorKind, Value};
use serde::Serialize;
use tokio::runtime::Handle;

const CALL_BUDGET: usize = 64;
const QUERY_BUDGET: usize = 10;
const DEADLINE: Duration = Duration::from_millis(500);

fn failure(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidOperation, message.into())
}

pub struct RenderScope {
    deadline: Instant,
    calls: AtomicUsize,
    queries: AtomicUsize,
    cache: Mutex<HashMap<String, Value>>,
    handle: Handle,
    data: Option<Arc<ThemeData>>,
    assets: Arc<HashMap<String, String>>,
}

impl RenderScope {
    pub fn new(
        data: Option<Arc<ThemeData>>,
        assets: Arc<HashMap<String, String>>,
    ) -> Result<Arc<Self>, UseCaseError> {
        Ok(Arc::new(Self {
            deadline: Instant::now() + DEADLINE,
            calls: AtomicUsize::new(0),
            queries: AtomicUsize::new(0),
            cache: Mutex::new(HashMap::new()),
            handle: Handle::try_current()
                .map_err(|e| UseCaseError::Render(format!("主题渲染缺少 Tokio runtime：{e}")))?,
            data,
            assets,
        }))
    }

    fn enter(&self) -> Result<(), Error> {
        if self.calls.fetch_add(1, Ordering::Relaxed) >= CALL_BUDGET {
            return Err(failure("主题函数调用预算耗尽"));
        }
        if Instant::now() >= self.deadline {
            return Err(failure("主题渲染已超时"));
        }
        Ok(())
    }

    fn data(&self) -> Result<Arc<ThemeData>, Error> {
        self.data
            .clone()
            .ok_or_else(|| failure("当前主题函数未装配公开数据源"))
    }

    fn query<T: Serialize, F: Future<Output = Result<T, UseCaseError>>>(
        &self,
        key: String,
        future: F,
    ) -> Result<Value, Error> {
        self.enter()?;
        if let Some(value) = self
            .cache
            .lock()
            .map_err(|_| failure("主题缓存锁失效"))?
            .get(&key)
            .cloned()
        {
            return Ok(value);
        }
        if self.queries.fetch_add(1, Ordering::Relaxed) >= QUERY_BUDGET {
            return Err(failure("主题数据查询预算耗尽"));
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(failure("主题渲染已超时"));
        }
        let result = self
            .handle
            .block_on(tokio::time::timeout(remaining, future))
            .map_err(|_| failure("主题数据查询超时"))?
            .map_err(|e| failure(format!("主题数据查询失败：{e}")))?;
        let value = Value::from_serialize(result);
        self.cache
            .lock()
            .map_err(|_| failure("主题缓存锁失效"))?
            .insert(key, value.clone());
        Ok(value)
    }
}

pub fn register(env: &mut Environment<'static>, scope: Arc<RenderScope>) {
    let posts_scope = scope.clone();
    env.add_function("get_posts", move |kwargs: Kwargs| -> Result<Value, Error> {
        let limit = kwargs.get::<Option<i64>>("limit")?.unwrap_or(10);
        let tag = kwargs.get::<Option<String>>("tag")?;
        let category = kwargs.get::<Option<String>>("category")?;
        kwargs.assert_all_used()?;
        if !(1..=50).contains(&limit) || (tag.is_some() && category.is_some()) {
            return Err(failure("get_posts 参数无效"));
        }
        let data = posts_scope.data()?;
        let key = format!("posts:{limit}:{tag:?}:{category:?}");
        posts_scope.query(key, async move {
            data.get_posts(limit, tag.as_deref(), category.as_deref())
                .await
        })
    });

    let post_scope = scope.clone();
    env.add_function("get_post", move |kwargs: Kwargs| -> Result<Value, Error> {
        let slug = kwargs.get::<String>("slug")?;
        kwargs.assert_all_used()?;
        let data = post_scope.data()?;
        post_scope.query(
            format!("post:{slug}"),
            async move { data.get_post(&slug).await },
        )
    });

    let categories_scope = scope.clone();
    env.add_function(
        "get_categories",
        move |kwargs: Kwargs| -> Result<Value, Error> {
            let limit = kwargs.get::<Option<i64>>("limit")?.unwrap_or(20);
            kwargs.assert_all_used()?;
            if !(1..=50).contains(&limit) {
                return Err(failure("get_categories limit 无效"));
            }
            let data = categories_scope.data()?;
            categories_scope.query(format!("categories:{limit}"), async move {
                data.get_categories(limit).await
            })
        },
    );

    let tags_scope = scope.clone();
    env.add_function("get_tags", move |kwargs: Kwargs| -> Result<Value, Error> {
        let limit = kwargs.get::<Option<i64>>("limit")?.unwrap_or(20);
        kwargs.assert_all_used()?;
        if !(1..=50).contains(&limit) {
            return Err(failure("get_tags limit 无效"));
        }
        let data = tags_scope.data()?;
        tags_scope.query(format!("tags:{limit}"), async move {
            data.get_tags(limit).await
        })
    });

    let asset_scope = scope.clone();
    env.add_function(
        "asset_url",
        move |kwargs: Kwargs| -> Result<String, Error> {
            let path = kwargs.get::<String>("path")?;
            kwargs.assert_all_used()?;
            asset_scope.enter()?;
            asset_scope
                .assets
                .get(&path)
                .cloned()
                .ok_or_else(|| failure("主题资源路径不存在或不安全"))
        },
    );

    let url_scope = scope;
    env.add_function("post_url", move |kwargs: Kwargs| -> Result<String, Error> {
        let slug = kwargs.get::<String>("slug")?;
        kwargs.assert_all_used()?;
        url_scope.enter()?;
        domain::content::post::Slug::new(&slug).map_err(|e| failure(e.to_string()))?;
        Ok(application::seo::post_path(&slug))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn pure_functions_reject_unknown_assets_and_bad_arguments() {
        tokio::task::spawn_blocking(|| {
            let mut assets = HashMap::new();
            assets.insert("style.css".into(), "/assets/style.css?v=123".into());
            let scope = RenderScope::new(None, Arc::new(assets)).unwrap();
            let mut env = Environment::new();
            register(&mut env, scope);
            env.add_template(
                "ok",
                "{{ post_url(slug='关于') }} {{ asset_url(path='style.css') }}",
            )
            .unwrap();
            let output = env.get_template("ok").unwrap().render(()).unwrap();
            assert!(output.contains("/posts/%E5%85%B3%E4%BA%8E"));
            assert!(output.contains("/assets/style.css?v=123"));
            env.add_template("missing", "{{ asset_url(path='../secret') }}")
                .unwrap();
            assert!(env.get_template("missing").unwrap().render(()).is_err());
            env.add_template("bad", "{{ get_posts(limit=51) }}")
                .unwrap();
            assert!(env.get_template("bad").unwrap().render(()).is_err());
            env.add_template("unknown", "{{ post_url(slug='valid', actor_id='x') }}")
                .unwrap();
            assert!(env.get_template("unknown").unwrap().render(()).is_err());
        })
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn function_call_budget_is_per_render() {
        tokio::task::spawn_blocking(|| {
            let mut env = Environment::new();
            let scope = RenderScope::new(None, Arc::new(HashMap::new())).unwrap();
            register(&mut env, scope);
            env.add_template(
                "loop",
                "{% for _ in range(65) %}{{ post_url(slug='a') }}{% endfor %}",
            )
            .unwrap();
            assert!(env.get_template("loop").unwrap().render(()).is_err());
            let mut another = Environment::new();
            register(
                &mut another,
                RenderScope::new(None, Arc::new(HashMap::new())).unwrap(),
            );
            another
                .add_template("one", "{{ post_url(slug='a') }}")
                .unwrap();
            assert_eq!(
                another.get_template("one").unwrap().render(()).unwrap(),
                "/posts/a"
            );
        })
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn repeated_query_uses_request_cache_and_distinct_queries_are_bounded() {
        tokio::task::spawn_blocking(|| {
            let scope = RenderScope::new(None, Arc::new(HashMap::new())).unwrap();
            for index in 0..QUERY_BUDGET {
                let key = format!("key-{index}");
                scope
                    .query(key.clone(), async move { Ok::<_, UseCaseError>(index) })
                    .unwrap();
                let cached = scope
                    .query(key, async move { Err::<usize, _>(UseCaseError::Forbidden) })
                    .unwrap();
                assert_eq!(cached, Value::from(index));
            }
            assert!(
                scope
                    .query("new-key".into(), async { Ok::<_, UseCaseError>(0usize) })
                    .is_err()
            );
        })
        .await
        .unwrap();
    }
}
