//! M0 主题桥接原型：MiniJinja 同步模板函数 ↔ 异步 sqlx 查询。
//!
//! 验证要点（对应 docs/themes-and-rendering.md §4）：
//! - 从 `spawn_blocking` 工作线程内经 `tokio::runtime::Handle::block_on` 驱动
//!   带 deadline 超时的 sqlx 查询 future；绝不新建 runtime，绝不在异步执行线程上 block_on。
//! - fuel（VM 指令计量）、递归深度、渲染截止时间、每页数据库查询次数预算、
//!   模板函数调用预算的组合限制；饱和时有限等待后返回受控错误。
//! - 请求级 RenderScope 通过 per-request `Environment` 副本（模板共享、函数表
//!   copy-on-write）+ 闭包捕获绑定，并行渲染之间无状态串用。
//! - 渲染许可信号量：异步侧有限等待获取，实际阻塞工作持有到退出，客户端
//!   超时放弃不提前释放许可。
//!
//! 本 crate 是与生产 workspace 隔离的实验工程，生产代码不得依赖。

pub mod pg;

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use minijinja::value::Kwargs;
use minijinja::{Environment, ErrorKind, Value};
use tokio::runtime::Handle;
use tokio::sync::Semaphore;

use pg::QueryFacade;

// ---------------------------------------------------------------------------
// 失败分类：所有失败路径都以受控 RenderFailure 返回，不允许 panic。
// ---------------------------------------------------------------------------

/// 受控失败类别；与 themes-and-rendering.md §4 的失败场景一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// 渲染截止时间已到（宿主调用入口检查失败）。
    DeadlineExceeded,
    /// 单次宿主查询在剩余 deadline 内未完成（tokio timeout 触发）。
    QueryTimeout,
    /// 每页独立数据库查询次数预算耗尽。
    QueryBudgetExhausted,
    /// 模板函数调用次数预算耗尽（缓存命中也计入调用预算）。
    CallBudgetExhausted,
    /// 数据库查询失败（连接/SQL 错误）。
    DbError,
    /// 渲染许可有限等待超时（并发饱和）。
    PermitWaitTimeout,
    /// MiniJinja fuel 耗尽。
    OutOfFuel,
    /// 递归深度超限。
    RecursionLimit,
    /// 其他模板错误（语法、未知参数、未知函数等）。
    TemplateError,
}

/// 渲染失败的受控表示；服务端 5xx 的雏形。
#[derive(Debug, Clone)]
pub struct RenderFailure {
    pub kind: FailureKind,
    pub message: String,
    /// 触发失败的宿主函数名（若来自宿主调用）。
    pub fn_name: Option<String>,
}

impl RenderFailure {
    fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            fn_name: None,
        }
    }

    fn at_fn(mut self, fn_name: &str) -> Self {
        self.fn_name = Some(fn_name.to_string());
        self
    }
}

impl std::fmt::Display for RenderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.fn_name {
            Some(name) => write!(f, "{:?}（{}）：{}", self.kind, name, self.message),
            None => write!(f, "{:?}：{}", self.kind, self.message),
        }
    }
}

/// 把 minijinja 错误映射为受控失败。宿主调用错误优先取 scope 内暂存的精确分类。
///
/// 错误可能被模板结构包装（如 include 嵌套把内层错误放进 source 链），
/// 因此沿 std::error::Error::source 链查找 fuel/递归错误的真实类别。
fn classify(err: minijinja::Error, stash: Option<RenderFailure>) -> RenderFailure {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(&err);
    while let Some(e) = cur {
        if let Some(mj) = e.downcast_ref::<minijinja::Error>() {
            if mj.kind() == ErrorKind::OutOfFuel {
                return RenderFailure::new(FailureKind::OutOfFuel, mj.to_string());
            }
            // 递归超限在 minijinja 中是 InvalidOperation + 固定消息。
            if mj.kind() == ErrorKind::InvalidOperation
                && mj.to_string().contains("recursion limit")
            {
                return RenderFailure::new(FailureKind::RecursionLimit, mj.to_string());
            }
        }
        cur = e.source();
    }
    stash.unwrap_or_else(|| RenderFailure::new(FailureKind::TemplateError, err.to_string()))
}

// ---------------------------------------------------------------------------
// 预算配置
// ---------------------------------------------------------------------------

/// 单次渲染（每页）的全部预算初值，供压测校准（themes-and-rendering.md §4：
/// 每页最多 10 次独立数据查询；命中请求缓存计入函数调用预算但不重复计查询）。
#[derive(Debug, Clone)]
pub struct Budgets {
    /// 渲染截止时间；传播到每次宿主查询的 tokio timeout。
    pub deadline: Duration,
    /// 每页独立数据库查询次数上限（默认 10）。
    pub query_budget: usize,
    /// 模板函数调用次数上限（缓存命中同样计入）。
    pub call_budget: usize,
    /// MiniJinja fuel（VM 指令计量，需要 fuel feature；不限制宿主函数内部 I/O）。
    pub fuel: Option<u64>,
    /// 递归深度上限（minijinja 无 stacker 特性时硬上限 500）。
    pub recursion_limit: usize,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            deadline: Duration::from_millis(500),
            query_budget: 10,
            call_budget: 64,
            fuel: Some(200_000),
            recursion_limit: 100,
        }
    }
}

// ---------------------------------------------------------------------------
// 请求作用域：deadline、预算、请求缓存、随机标记
// ---------------------------------------------------------------------------

/// 一次渲染请求的全部可变状态；经 Arc 进入模板函数闭包。
/// 观测计数器（queries_issued/cache_hits/calls）公开供测试与计时断言使用。
pub struct ScopeState {
    /// 每请求随机标记，验证并行渲染之间无状态串用。
    pub marker: String,
    deadline: Instant,
    queries_left: AtomicI64,
    calls_left: AtomicI64,
    cache: Mutex<HashMap<String, Value>>,
    handle: Handle,
    query: QueryFacade,
    /// 观测：实际打到数据库的查询次数（miss）。
    pub queries_issued: AtomicUsize,
    /// 观测：请求缓存命中次数。
    pub cache_hits: AtomicUsize,
    /// 观测：宿主函数调用总次数。
    pub calls: AtomicUsize,
    /// 最近一次宿主调用失败的精确分类（minijinja 错误只有字符串）。
    last_host_error: Mutex<Option<RenderFailure>>,
}

impl ScopeState {
    /// 宿主调用统一入口：调用预算 → deadline → 请求缓存 → 查询预算 → block_on。
    ///
    /// 只会被渲染线程（spawn_blocking 工作线程）同步调用；
    /// `Handle::block_on` 在阻塞线程上驱动查询 future 是 tokio 认可的桥接方式。
    pub fn host_call(&self, fn_name: &str, key: &str) -> Result<Value, minijinja::Error> {
        // 1. 函数调用预算：缓存命中也计入。
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.calls_left.fetch_sub(1, Ordering::Relaxed) <= 0 {
            return Err(self.fail(
                fn_name,
                FailureKind::CallBudgetExhausted,
                "模板函数调用次数预算耗尽",
            ));
        }
        // 2. 截止时间检查（缓存命中也要受 deadline 约束）。
        let Some(remaining) = self.deadline.checked_duration_since(Instant::now()) else {
            return Err(self.fail(fn_name, FailureKind::DeadlineExceeded, "渲染截止时间已到"));
        };
        // 3. 请求级缓存。
        {
            let cache = self.cache.lock().expect("请求缓存锁中毒");
            if let Some(v) = cache.get(key) {
                self.cache_hits.fetch_add(1, Ordering::Relaxed);
                return Ok(v.clone());
            }
        }
        // 4. 数据库查询预算：仅 miss 计费。
        if self.queries_left.fetch_sub(1, Ordering::Relaxed) <= 0 {
            return Err(self.fail(
                fn_name,
                FailureKind::QueryBudgetExhausted,
                "每页数据库查询次数预算耗尽",
            ));
        }
        self.queries_issued.fetch_add(1, Ordering::Relaxed);
        // 5. 从阻塞线程用 Handle 驱动带 deadline 超时的查询 future。
        let fut = (self.query)(key);
        let outcome = self
            .handle
            .block_on(async { tokio::time::timeout(remaining, fut).await });
        match outcome {
            Err(_elapsed) => Err(self.fail(
                fn_name,
                FailureKind::QueryTimeout,
                "宿主查询在截止时间内未完成",
            )),
            Ok(Err(db_err)) => Err(self.fail(
                fn_name,
                FailureKind::DbError,
                format!("数据库查询失败：{db_err}"),
            )),
            Ok(Ok(value)) => {
                self.cache
                    .lock()
                    .expect("请求缓存锁中毒")
                    .insert(key.to_string(), value.clone());
                Ok(value)
            }
        }
    }

    /// 构造受控错误：写入暂存并返回带前缀的 minijinja 错误。
    fn fail(&self, fn_name: &str, kind: FailureKind, msg: impl Into<String>) -> minijinja::Error {
        let failure = RenderFailure::new(kind, msg).at_fn(fn_name);
        *self.last_host_error.lock().expect("宿主错误暂存锁中毒") = Some(failure.clone());
        minijinja::Error::new(
            ErrorKind::InvalidOperation,
            format!("[bridge:{:?}] {}", failure.kind, failure.message),
        )
    }

    pub fn take_stashed_failure(&self) -> Option<RenderFailure> {
        self.last_host_error
            .lock()
            .expect("宿主错误暂存锁中毒")
            .take()
    }
}

/// 请求级渲染作用域：不可变预算 + 可变状态。每请求独立创建，绝不跨请求共享。
pub struct RenderScope {
    state: Arc<ScopeState>,
    budgets: Budgets,
}

impl RenderScope {
    pub fn new(handle: Handle, facade: QueryFacade, marker: String, budgets: Budgets) -> Self {
        let state = ScopeState {
            marker,
            deadline: Instant::now() + budgets.deadline,
            queries_left: AtomicI64::new(budgets.query_budget as i64),
            calls_left: AtomicI64::new(budgets.call_budget as i64),
            cache: Mutex::new(HashMap::new()),
            handle,
            query: facade,
            queries_issued: AtomicUsize::new(0),
            cache_hits: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            last_host_error: Mutex::new(None),
        };
        Self {
            state: Arc::new(state),
            budgets,
        }
    }

    pub fn marker(&self) -> &str {
        &self.state.marker
    }

    pub fn state(&self) -> Arc<ScopeState> {
        self.state.clone()
    }

    pub fn budgets(&self) -> &Budgets {
        &self.budgets
    }
}

// ---------------------------------------------------------------------------
// 请求函数注册：per-request Environment 副本 + 闭包捕获 scope
// ---------------------------------------------------------------------------

/// 在 per-request Environment 副本上注册模板函数。
///
/// `Environment::clone` 共享已编译模板（Arc），filters/globals 为 Arc + copy-on-write，
/// 副本上 add_function 不会影响基础环境或其他请求的副本（测试验证）。
pub fn register_request_functions(env: &mut Environment<'static>, state: Arc<ScopeState>) {
    let s = state.clone();
    env.add_function(
        "get_posts",
        move |kwargs: Kwargs| -> Result<Value, minijinja::Error> {
            // 关键字参数 + 未知参数拒绝：与主题 API 草案（get_posts(limit=…)）一致。
            let limit: Option<usize> = kwargs.get("limit")?;
            let offset: Option<usize> = kwargs.get("offset")?;
            kwargs.assert_all_used()?;
            let limit = limit.unwrap_or(10).clamp(1, 50);
            let offset = offset.unwrap_or(0);
            s.host_call("get_posts", &format!("posts:{limit}:{offset}"))
        },
    );

    let s = state.clone();
    env.add_function("whoami", move || -> Value {
        // 纯闭包绑定（不查库）：返回请求随机标记，验证请求隔离。
        Value::from(s.marker.clone())
    });

    let s = state;
    env.add_function(
        "raw_query",
        move |key: String| -> Result<Value, minijinja::Error> {
            // 原型后门：把任意 key 交给查询门面（posts:/sleep:/boom:/echo:），
            // 供失败场景与预算测试直接控制查询行为。
            s.host_call("raw_query", &key)
        },
    );
}

// ---------------------------------------------------------------------------
// 渲染桥：异步侧许可 → spawn_blocking 渲染 → 函数内 block_on 查询
// ---------------------------------------------------------------------------

/// 阻塞线程占用守卫：即使渲染 panic 也保证观测计数归位。
struct ActiveGuard(Arc<AtomicI64>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 渲染桥：持有模板与全局配置；每请求 clone 基础环境并绑定请求 scope。
pub struct Bridge {
    base: Environment<'static>,
    permits: Arc<Semaphore>,
    permit_wait: Duration,
    /// 观测：当前占用阻塞线程的渲染数（验证截止时间后线程确实退出）。
    active_blocking: Arc<AtomicI64>,
}

impl Bridge {
    /// `templates` 为 (名称, 源码)；源码按生产 rendering.rs 的方式泄漏为 'static。
    pub fn new(
        templates: &[(&str, &str)],
        render_concurrency: usize,
        permit_wait: Duration,
    ) -> Result<Self, String> {
        let mut base = Environment::new();
        for (name, src) in templates {
            let name: &'static str = Box::leak((*name).to_string().into_boxed_str());
            let leaked: &'static str = Box::leak((*src).to_string().into_boxed_str());
            base.add_template(name, leaked)
                .map_err(|e| format!("模板 {name} 解析失败：{e}"))?;
        }
        Ok(Self {
            base,
            permits: Arc::new(Semaphore::new(render_concurrency)),
            permit_wait,
            active_blocking: Arc::new(AtomicI64::new(0)),
        })
    }

    /// 观测句柄：阻塞渲染占用数。
    pub fn active_blocking(&self) -> Arc<AtomicI64> {
        self.active_blocking.clone()
    }

    /// 观测句柄：渲染许可信号量（验证许可持有到工作退出）。
    pub fn permits(&self) -> Arc<Semaphore> {
        self.permits.clone()
    }

    /// 异步侧入口：有限等待获取许可后把同步渲染送入阻塞池，返回 JoinHandle。
    ///
    /// 许可随阻塞任务持有到退出：客户端放弃 JoinHandle 不会提前释放许可，
    /// 截止时间在每次宿主调用内部传播，阻塞线程最终自行退出。
    pub async fn spawn_render(
        &self,
        scope: RenderScope,
        template: &str,
        ctx: serde_json::Value,
    ) -> Result<tokio::task::JoinHandle<Result<String, RenderFailure>>, RenderFailure> {
        // 饱和：有限等待后受控失败，不无限排队。
        // 注：tokio 1.53 已移除 Semaphore::acquire_timeout，用 timeout 包 acquire_owned
        // 等价实现有限等待；超时取消不会丢失许可。OwnedSemaphorePermit 可移入阻塞任务。
        let permit = tokio::time::timeout(self.permit_wait, self.permits.clone().acquire_owned())
            .await
            .map_err(|_| {
                RenderFailure::new(
                    FailureKind::PermitWaitTimeout,
                    format!("渲染许可有限等待超时（等待 {:?}）", self.permit_wait),
                )
            })?
            .map_err(|e| {
                RenderFailure::new(
                    FailureKind::PermitWaitTimeout,
                    format!("渲染许可信号量已关闭：{e}"),
                )
            })?;

        self.active_blocking.fetch_add(1, Ordering::SeqCst);
        let base = self.base.clone();
        let active = self.active_blocking.clone();
        let template = template.to_string();
        let state = scope.state();
        let budgets = scope.budgets().clone();

        // 许可与占用守卫都移入闭包，持有到渲染退出。
        Ok(tokio::task::spawn_blocking(move || {
            let _active = ActiveGuard(active);
            let _permit_held = permit;
            let mut env = base.clone();
            register_request_functions(&mut env, state.clone());
            if let Some(fuel) = budgets.fuel {
                env.set_fuel(Some(fuel));
            }
            env.set_recursion_limit(budgets.recursion_limit);
            env.get_template(&template)
                .and_then(|t| t.render(ctx))
                .map_err(|e| classify(e, state.take_stashed_failure()))
        }))
    }

    /// 完整链路：spawn_render 后等待结果。JoinError（渲染 panic）也映射为受控失败。
    pub async fn render(
        &self,
        scope: RenderScope,
        template: &str,
        ctx: serde_json::Value,
    ) -> Result<String, RenderFailure> {
        let handle = self.spawn_render(scope, template, ctx).await?;
        match handle.await {
            Ok(result) => result,
            Err(join_err) => Err(RenderFailure::new(
                FailureKind::TemplateError,
                format!("渲染阻塞任务异常结束：{join_err}"),
            )),
        }
    }
}
