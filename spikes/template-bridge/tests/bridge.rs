//! M0 主题桥接原型集成测试：验证 themes-and-rendering.md §4 的核心问题。
//!
//! 全部为真实 PostgreSQL 集成测试（破坏性重建 spike_bridge_test 库），
//! 库不可达或非 loopback 时直接 panic 失败，不允许静默跳过。
//! 每个 `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]` 驱动完整链路：异步侧获取许可 → spawn_blocking 渲染
//! → 函数内 Handle::block_on 查询。

use std::sync::Arc;
use std::time::{Duration, Instant};

use minijinja::Environment;
use template_bridge::pg::{self, QueryFacade};
use template_bridge::{Bridge, Budgets, FailureKind, RenderScope};
use tokio::runtime::Handle;
use uuid::Uuid;

/// 测试库 DSN 只初始化一次（破坏性重建）；每个测试自建连接池，
/// 避免共享池跨 `#[tokio::test]` runtime（见 pg::recreate_database 文档）。
static DSN: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();
/// 测试串行锁：真实库共享，避免 pg_sleep 占用连接互相干扰计时断言（与仓库 PG 测试同风格）。
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn pool() -> sqlx::PgPool {
    let dsn = DSN
        .get_or_init(|| async { pg::recreate_database().await })
        .await;
    pg::connect_test(dsn, 16).await
}

fn bridge(templates: &[(&str, &str)], concurrency: usize, permit_wait: Duration) -> Bridge {
    Bridge::new(templates, concurrency, permit_wait).expect("构建渲染桥失败")
}

fn scope(facade: &QueryFacade, marker: &str, budgets: Budgets) -> RenderScope {
    RenderScope::new(
        Handle::current(),
        facade.clone(),
        marker.to_string(),
        budgets,
    )
}

fn default_budgets() -> Budgets {
    Budgets::default()
}

/// 等待观测条件成立（轮询），用于验证截止时间后阻塞线程确实退出。
async fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    f()
}

// ---------------------------------------------------------------------------
// 1. 同步-异步桥接基本链路
// ---------------------------------------------------------------------------

/// 完整链路：spawn_blocking 渲染，函数内 block_on 驱动真实 sqlx 查询。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_chain_renders_rows_via_block_on() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, facade_queries) = pg::query_facade(pool);
    let tpl = BridgeTemplate::LIST;
    let bridge = bridge(&[tpl], 4, Duration::from_secs(1));

    let sc = scope(&facade, "m-basic", default_budgets());
    let out = bridge
        .render(sc, "list.html", serde_json::json!({}))
        .await
        .expect("渲染失败");

    assert_eq!(out, "post-00;post-01;post-02;post-03;post-04;");
    assert_eq!(facade_queries.load(std::sync::atomic::Ordering::Relaxed), 1);
}

/// 同参数重复调用命中请求缓存：不重复查询，但计入函数调用预算。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_cache_dedupes_but_counts_calls() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, _) = pg::query_facade(pool);
    let bridge = bridge(
        &[(
            "dedupe.html",
            r#"{% set a = raw_query("posts:3:0") %}{% set b = raw_query("posts:3:0") %}{{ a.items|length }}-{{ b.items|length }}"#,
        )],
        4,
        Duration::from_secs(1),
    );

    let sc = scope(&facade, "m-cache", default_budgets());
    let state = sc.state();
    let out = bridge
        .render(sc, "dedupe.html", serde_json::json!({}))
        .await
        .expect("渲染失败");

    assert_eq!(out, "3-3");
    assert_eq!(
        state
            .queries_issued
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "同参数重复调用不应重复查询"
    );
    assert_eq!(
        state.cache_hits.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert_eq!(
        state.calls.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "缓存命中仍计入调用预算"
    );
}

/// 预取上下文渲染与函数取数渲染输出一致（性能对照的正确性前提）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn prefetch_and_function_output_match() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, _) = pg::query_facade(pool.clone());
    let bridge = bridge(
        &[BridgeTemplate::LIST, BridgeTemplate::CTX],
        4,
        Duration::from_secs(1),
    );

    let sc = scope(&facade, "m-match", default_budgets());
    let via_fn = bridge
        .render(sc, "list.html", serde_json::json!({}))
        .await
        .expect("函数渲染失败");

    // 模拟 application 层异步预取：在异步上下文直接 await 查询门面。
    let prefetched = facade("posts:5:0").await.expect("预取查询失败");
    let items: Vec<serde_json::Value> =
        serde_json::to_value(&prefetched).expect("序列化失败")["items"]
            .as_array()
            .expect("items 不是数组")
            .clone();
    let via_ctx = bridge
        .render(
            scope(&facade, "m-match2", default_budgets()),
            "ctx.html",
            serde_json::json!({ "posts": items }),
        )
        .await
        .expect("预取渲染失败");
    assert_eq!(via_fn, via_ctx);
}

// ---------------------------------------------------------------------------
// 2. 预算与截止时间
// ---------------------------------------------------------------------------

/// 循环内唯一查询超过每页数据库查询预算：受控失败，不 panic。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn query_budget_exhaustion_is_controlled() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, _) = pg::query_facade(pool);
    let bridge = bridge(
        &[("miss_loop.html", BridgeTemplate::MISS_LOOP)],
        4,
        Duration::from_secs(1),
    );

    let budgets = Budgets {
        query_budget: 3,
        call_budget: 100,
        fuel: Some(10_000_000),
        ..default_budgets()
    };
    let sc = scope(&facade, "m-budget", budgets);
    let state = sc.state();
    let err = bridge
        .render(sc, "miss_loop.html", serde_json::json!({}))
        .await
        .expect_err("超出查询预算应当失败");

    assert_eq!(
        err.kind,
        FailureKind::QueryBudgetExhausted,
        "实际错误：{err}"
    );
    assert_eq!(err.fn_name.as_deref(), Some("raw_query"));
    assert_eq!(
        state
            .queries_issued
            .load(std::sync::atomic::Ordering::Relaxed),
        3,
        "恰好在第 4 次独立查询前被预算拦截"
    );
}

/// 缓存命中不消耗查询预算；函数调用预算独立计数。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cache_hits_do_not_burn_query_budget() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, _) = pg::query_facade(pool);
    let bridge = bridge(
        &[("hit_loop.html", BridgeTemplate::HIT_LOOP)],
        4,
        Duration::from_secs(1),
    );

    // 查询预算 1，5 次同参数调用：1 次 miss + 4 次缓存命中，全部成功。
    let budgets = Budgets {
        query_budget: 1,
        call_budget: 10,
        fuel: Some(10_000_000),
        ..default_budgets()
    };
    let sc = scope(&facade, "m-hit", budgets);
    let state = sc.state();
    let out = bridge
        .render(sc, "hit_loop.html", serde_json::json!({}))
        .await
        .expect("缓存命中不应耗尽查询预算");
    assert_eq!(out, "post-00");
    assert_eq!(
        state
            .queries_issued
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );

    // 调用预算 3：第 4 次调用（即使会命中缓存）受控失败。
    let budgets = Budgets {
        query_budget: 10,
        call_budget: 3,
        fuel: Some(10_000_000),
        ..default_budgets()
    };
    let sc = scope(&facade, "m-call", budgets);
    let err = bridge
        .render(sc, "hit_loop.html", serde_json::json!({}))
        .await
        .expect_err("调用预算耗尽应当失败");
    assert_eq!(
        err.kind,
        FailureKind::CallBudgetExhausted,
        "实际错误：{err}"
    );
}

/// 慢查询（pg_sleep 500ms）在 100ms deadline 下：在 deadline 附近受控失败，
/// 不会等到查询自然结束，也不无限占用阻塞线程。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_query_deadline_fails_at_deadline_not_after() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, _) = pg::query_facade(pool);
    let bridge = bridge(
        &[("sleep.html", BridgeTemplate::SLEEP_500)],
        4,
        Duration::from_secs(1),
    );
    let active = bridge.active_blocking();

    let budgets = Budgets {
        deadline: Duration::from_millis(100),
        fuel: Some(10_000_000),
        ..default_budgets()
    };
    let sc = scope(&facade, "m-deadline", budgets);

    let start = Instant::now();
    let err = bridge
        .render(sc, "sleep.html", serde_json::json!({}))
        .await
        .expect_err("慢查询应超时");
    let elapsed = start.elapsed();

    assert_eq!(err.kind, FailureKind::QueryTimeout, "实际错误：{err}");
    assert!(
        elapsed < Duration::from_millis(300),
        "失败应在 deadline(100ms) 附近传播，实际 {elapsed:?}（自然结束需 500ms）"
    );
    // 截止时间后阻塞线程确实退出（spawn_blocking 不可 abort，只能自行退出）。
    assert!(
        wait_until(Duration::from_secs(2), || active
            .load(std::sync::atomic::Ordering::SeqCst)
            == 0)
        .await,
        "截止时间后阻塞渲染线程未退出"
    );
}

/// deadline 在宿主调用入口之间也检查：高频廉价调用下，deadline 一到即失败。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deadline_checked_between_host_calls() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, _) = pg::query_facade(pool);
    let bridge = bridge(
        &[("echo_loop.html", BridgeTemplate::ECHO_LOOP)],
        4,
        Duration::from_secs(1),
    );

    let budgets = Budgets {
        deadline: Duration::from_millis(60),
        call_budget: 1_000_000,
        fuel: Some(50_000_000),
        ..default_budgets()
    };
    let sc = scope(&facade, "m-echo", budgets);
    let err = bridge
        .render(sc, "echo_loop.html", serde_json::json!({}))
        .await
        .expect_err("廉价调用循环也应在 deadline 后停止");
    assert_eq!(err.kind, FailureKind::DeadlineExceeded, "实际错误：{err}");
}

/// fuel 耗尽：受控错误。fuel 只按 VM 指令计量，纯循环立刻被拦。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fuel_exhaustion_is_controlled() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, _) = pg::query_facade(pool);
    let bridge = bridge(
        &[("spin.html", BridgeTemplate::SPIN)],
        4,
        Duration::from_secs(1),
    );

    let budgets = Budgets {
        fuel: Some(50_000),
        ..default_budgets()
    };
    let sc = scope(&facade, "m-fuel", budgets);
    let start = Instant::now();
    let err = bridge
        .render(sc, "spin.html", serde_json::json!({}))
        .await
        .expect_err("空循环应耗尽 fuel");
    assert_eq!(err.kind, FailureKind::OutOfFuel, "实际错误：{err}");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "fuel 耗尽应立即失败"
    );
}

/// fuel 不限制宿主函数内部 I/O：fuel=50 时一次 300ms 数据库睡眠查询照常完成。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fuel_does_not_limit_host_io() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, _) = pg::query_facade(pool);
    let bridge = bridge(
        &[
            ("sleep300.html", BridgeTemplate::SLEEP_300),
            ("spin.html", BridgeTemplate::SPIN),
        ],
        4,
        Duration::from_secs(1),
    );

    // 极小 fuel 下，宿主函数内的 300ms I/O 完整执行。
    let budgets = Budgets {
        fuel: Some(50),
        deadline: Duration::from_secs(5),
        ..default_budgets()
    };
    let sc = scope(&facade, "m-io", budgets);
    let start = Instant::now();
    let out = bridge
        .render(sc, "sleep300.html", serde_json::json!({}))
        .await
        .expect("fuel 不应限制宿主 I/O");
    assert_eq!(out, "300");
    assert!(
        start.elapsed() >= Duration::from_millis(250),
        "宿主 I/O 确实执行了 300ms：{:?}",
        start.elapsed()
    );

    // 同样 fuel=50，纯 VM 循环立即被拦：证明 fuel 计量的是指令而非时间。
    let sc = scope(
        &facade,
        "m-io2",
        Budgets {
            fuel: Some(50),
            ..default_budgets()
        },
    );
    let err = bridge
        .render(sc, "spin.html", serde_json::json!({}))
        .await
        .expect_err("空循环应耗尽 fuel");
    assert_eq!(err.kind, FailureKind::OutOfFuel);
}

/// 递归过深（include 自包含）：受控错误，无栈溢出崩溃。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recursion_limit_is_controlled() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, _) = pg::query_facade(pool);
    let bridge = bridge(
        &[("recurse.html", BridgeTemplate::RECURSE)],
        4,
        Duration::from_secs(1),
    );

    let budgets = Budgets {
        recursion_limit: 50,
        ..default_budgets()
    };
    let sc = scope(&facade, "m-rec", budgets);
    let err = bridge
        .render(sc, "recurse.html", serde_json::json!({}))
        .await
        .expect_err("自包含递归应触发深度限制");
    assert_eq!(err.kind, FailureKind::RecursionLimit, "实际错误：{err}");
    assert!(
        err.message.contains("recursion limit"),
        "消息：{}",
        err.message
    );
}

/// 输出无原生大小限制：fuel 很小时 5MB 单值输出仍完整渲染。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn output_has_no_native_size_cap() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, _) = pg::query_facade(pool);
    let bridge = bridge(
        &[("big.html", BridgeTemplate::BIG)],
        4,
        Duration::from_secs(1),
    );

    let budgets = Budgets {
        fuel: Some(1_000),
        ..default_budgets()
    };
    let sc = scope(&facade, "m-big", budgets);
    let out = bridge
        .render(
            sc,
            "big.html",
            serde_json::json!({ "big": "x".repeat(5_000_000) }),
        )
        .await
        .expect("单值输出不受 fuel/输出上限拦截");
    assert_eq!(out.len(), 5_000_000);
}

// ---------------------------------------------------------------------------
// 3. 失败场景：数据库错误、许可饱和、客户端放弃
// ---------------------------------------------------------------------------

/// 查询报错（SELECT 1/0）：受控 DbError，不 panic。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn db_error_is_controlled() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, _) = pg::query_facade(pool);
    let bridge = bridge(
        &[("boom.html", BridgeTemplate::BOOM)],
        4,
        Duration::from_secs(1),
    );

    let sc = scope(&facade, "m-boom", default_budgets());
    let err = bridge
        .render(sc, "boom.html", serde_json::json!({}))
        .await
        .expect_err("SQL 错误应当失败");
    assert_eq!(err.kind, FailureKind::DbError, "实际错误：{err}");
    assert!(
        err.message.contains("division by zero"),
        "消息：{}",
        err.message
    );
}

/// 许可饱和：2 个许可被慢渲染占用时，第 3 个在有限等待后受控失败；
/// 占用者正常完成后许可释放。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn permit_saturation_fails_after_bounded_wait() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, _) = pg::query_facade(pool);
    let bridge = bridge(
        &[("sleep400.html", BridgeTemplate::SLEEP_400)],
        2,
        Duration::from_millis(80),
    );
    let permits = bridge.permits();
    let active = bridge.active_blocking();

    // 两个占用者：慢渲染（deadline 足够长，确保它们能自然完成）。
    let holder_budgets = Budgets {
        deadline: Duration::from_secs(5),
        fuel: Some(10_000_000),
        ..default_budgets()
    };
    let a = bridge
        .spawn_render(
            scope(&facade, "m-a", holder_budgets.clone()),
            "sleep400.html",
            serde_json::json!({}),
        )
        .await
        .expect("A 应获得许可");
    let b = bridge
        .spawn_render(
            scope(&facade, "m-b", holder_budgets),
            "sleep400.html",
            serde_json::json!({}),
        )
        .await
        .expect("B 应获得许可");
    tokio::time::sleep(Duration::from_millis(50)).await; // 确保占用者已入场

    let start = Instant::now();
    let err = bridge
        .render(
            scope(&facade, "m-c", default_budgets()),
            "sleep400.html",
            serde_json::json!({}),
        )
        .await
        .expect_err("饱和时第三路应有限等待失败");
    let waited = start.elapsed();

    assert_eq!(err.kind, FailureKind::PermitWaitTimeout, "实际错误：{err}");
    assert!(
        waited < Duration::from_millis(400),
        "有限等待应在占用者结束(≈400ms)前返回：{waited:?}"
    );

    let (ra, rb) = tokio::join!(a, b);
    ra.expect("A 无 panic").expect("A 应正常完成");
    rb.expect("B 无 panic").expect("B 应正常完成");
    assert_eq!(permits.available_permits(), 2, "许可应全部归还");
    assert!(
        wait_until(Duration::from_secs(2), || active
            .load(std::sync::atomic::Ordering::SeqCst)
            == 0)
        .await,
        "阻塞渲染线程应全部退出"
    );
}

/// 客户端提前放弃：许可与线程不提前释放、也不永久泄漏——
/// 截止时间传播进宿主调用后，阻塞线程自行退出并归还许可。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_abandon_does_not_leak_worker_or_permit() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, _) = pg::query_facade(pool);
    let bridge = bridge(
        &[("sleep400.html", BridgeTemplate::SLEEP_400)],
        1,
        Duration::from_secs(5),
    );
    let permits = bridge.permits();
    let active = bridge.active_blocking();

    // 内部 deadline 120ms，宿主查询 400ms：客户端 40ms 就放弃。
    let budgets = Budgets {
        deadline: Duration::from_millis(120),
        fuel: Some(10_000_000),
        ..default_budgets()
    };
    let handle = bridge
        .spawn_render(
            scope(&facade, "m-abandon", budgets),
            "sleep400.html",
            serde_json::json!({}),
        )
        .await
        .expect("应获得许可");

    let start = Instant::now();
    let client = tokio::time::timeout(Duration::from_millis(40), handle).await;
    assert!(client.is_err(), "客户端在 40ms 放弃");
    // 此刻许可仍被工作持有（未提前释放），阻塞线程仍在运行。
    assert_eq!(permits.available_permits(), 0, "客户端放弃不应提前释放许可");
    assert_eq!(active.load(std::sync::atomic::Ordering::SeqCst), 1);

    // 截止时间(120ms)后线程自行退出、许可归还。
    assert!(
        wait_until(Duration::from_secs(2), || {
            permits.available_permits() == 1
                && active.load(std::sync::atomic::Ordering::SeqCst) == 0
        })
        .await,
        "截止时间后阻塞线程未退出或许可未归还（耗时 {:?}）",
        start.elapsed()
    );

    // 后续渲染能正常获得许可，证明无泄漏。
    let out = bridge
        .render(
            scope(&facade, "m-after", default_budgets()),
            "sleep400.html",
            serde_json::json!({}),
        )
        .await;
    // 500ms 默认 deadline < 400ms 查询，应当成功。
    assert_eq!(out.expect("后续渲染应成功"), "400");
}

// ---------------------------------------------------------------------------
// 4. 请求隔离
// ---------------------------------------------------------------------------

/// 32 路并行渲染，每请求注入独立随机标记：输出互不串用；
/// 每请求缓存各自独立（同一键全局只查一次/请求）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_requests_no_state_crossing() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, facade_queries) = pg::query_facade(pool);

    let markers: Vec<String> = (0..32).map(|_| Uuid::new_v4().to_string()).collect();
    // tokio::spawn 并发驱动；顺序 await JoinHandle 只是收集结果，不串行执行。
    let bridge = Arc::new(bridge(
        &[("dbl.html", BridgeTemplate::DBL)],
        8,
        Duration::from_secs(5),
    ));
    let mut handles = Vec::new();
    for marker in markers.iter() {
        let sc = scope(&facade, marker, default_budgets());
        let bridge = bridge.clone();
        handles.push(tokio::spawn(async move {
            bridge.render(sc, "dbl.html", serde_json::json!({})).await
        }));
    }
    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        results.push(h.await.expect("任务无 panic"));
    }

    let mut seen = std::collections::HashSet::new();
    for (i, result) in results.into_iter().enumerate() {
        let out = result.expect("并行渲染不应失败");
        let marker = &markers[i];
        // 模板首尾各输出一次 whoami()：都应是本请求自己的标记。
        let expected_prefix = format!("{marker}|post-00|2|");
        assert!(
            out.starts_with(&expected_prefix),
            "输出 {out:?} 未携带本请求标记 {marker}"
        );
        assert!(
            out.ends_with(&format!("|{marker}")),
            "输出结尾标记串用：{out:?}"
        );
        assert!(seen.insert(out.clone()), "并行输出不应完全相同：{out:?}");
    }
    assert_eq!(seen.len(), 32, "32 路输出应全部不同");
    // 每请求 2 次同键调用 → 全局恰 32 次查询 + 32 次请求缓存命中。
    assert_eq!(
        facade_queries.load(std::sync::atomic::Ordering::Relaxed),
        32
    );
}

/// Environment 副本的 add_function 是 copy-on-write：
/// 副本上的函数不泄漏回基础环境或其他副本（per-request env 方案的前提）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn env_clone_add_function_is_isolated() {
    let mut base = Environment::new();
    base.add_template("who.html", "{{ whoami() }}")
        .expect("模板解析失败");

    let mut clone_a = base.clone();
    clone_a.add_function("whoami", || -> String { "alpha".into() });
    let mut clone_b = base.clone();
    clone_b.add_function("whoami", || -> String { "beta".into() });

    let a = clone_a
        .get_template("who.html")
        .unwrap()
        .render(())
        .unwrap();
    let b = clone_b
        .get_template("who.html")
        .unwrap()
        .render(())
        .unwrap();
    assert_eq!(a, "alpha");
    assert_eq!(b, "beta");
    // 基础环境未被污染：whoami 未注册。
    let base_err = base
        .get_template("who.html")
        .unwrap()
        .render(())
        .unwrap_err();
    assert!(
        base_err.to_string().contains("whoami"),
        "基础环境不应获得副本函数：{base_err}"
    );
}

/// 未知关键字参数被拒绝（受控模板错误，符合主题 API 约束）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_kwargs_are_rejected() {
    let _serial = SERIAL.lock().await;
    let pool = pool().await;
    let (facade, _) = pg::query_facade(pool);
    let bridge = bridge(
        &[("kwargs.html", BridgeTemplate::KWARGS)],
        4,
        Duration::from_secs(1),
    );

    let sc = scope(&facade, "m-kwargs", default_budgets());
    let err = bridge
        .render(sc, "kwargs.html", serde_json::json!({}))
        .await
        .expect_err("未知参数应被拒绝");
    assert_eq!(err.kind, FailureKind::TemplateError);
    assert!(err.message.contains("bogus"), "消息：{}", err.message);
}

// ---------------------------------------------------------------------------
// 辅助：模板集与简易 join
// ---------------------------------------------------------------------------

struct BridgeTemplate;
impl BridgeTemplate {
    /// 函数取数：get_posts(limit=5) → 5 篇已发布文章 slug。
    const LIST: (&'static str, &'static str) = (
        "list.html",
        r#"{% set r = get_posts(limit=5) %}{% for p in r.items %}{{ p.slug }};{% endfor %}"#,
    );
    /// 纯预取上下文：同一批 slug 由上下文变量给出。
    const CTX: (&'static str, &'static str) = (
        "ctx.html",
        r#"{% for p in posts %}{{ p.slug }};{% endfor %}"#,
    );
    /// 10 个不同查询键（查询预算测试）。
    const MISS_LOOP: &'static str =
        r#"{% for i in range(10) %}{{ raw_query("posts:1:" ~ i).items[0].slug }}{% endfor %}"#;
    /// 5 次同参数调用（缓存/调用预算测试）。注意 Jinja 的 for 作用域：
    /// 循环内 set 不外溢，因此先在循环外取一次供输出，循环内再重复 4 次。
    const HIT_LOOP: &'static str = r#"{{ raw_query("posts:1:0").items[0].slug }}{% for i in range(4) %}{% set _ = raw_query("posts:1:0") %}{% endfor %}"#;
    const SLEEP_500: &'static str = r#"{{ raw_query("sleep:500") }}"#;
    const SLEEP_400: &'static str = r#"{{ raw_query("sleep:400") }}"#;
    const SLEEP_300: &'static str = r#"{{ raw_query("sleep:300") }}"#;
    /// 高频廉价宿主调用（deadline 入口检查测试）。
    const ECHO_LOOP: &'static str =
        r#"{% for i in range(100000) %}{% set x = raw_query("echo:x") %}{% endfor %}done"#;
    /// 纯 VM 循环（fuel 测试）。minijinja 的 range 内建上限 100000 元素，
    /// 用嵌套循环达到百万次迭代。
    const SPIN: &'static str =
        r#"{% for i in range(1000) %}{% for j in range(1000) %}{% endfor %}{% endfor %}done"#;
    /// 自包含递归（递归深度测试）。
    const RECURSE: &'static str = r#"rec{% include "recurse.html" %}"#;
    /// 单值大输出（输出上限测试）。
    const BIG: &'static str = "{{ big }}";
    /// 必然 SQL 错误。
    const BOOM: &'static str = r#"{{ raw_query("boom") }}"#;
    /// 隔离验证：首尾标记 + 两次同键查询。
    const DBL: &'static str = r#"{{ whoami() }}|{{ raw_query("posts:2:0").items[0].slug }}|{{ raw_query("posts:2:0").items|length }}|{{ whoami() }}"#;
    /// 未知关键字参数。
    const KWARGS: &'static str = r#"{% set r = get_posts(limit=2, bogus=3) %}x"#;
}
