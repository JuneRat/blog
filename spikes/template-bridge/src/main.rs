//! M0 主题桥接原型：性能对照与失败传播计时（release 运行）。
//!
//! cargo run --release
//!
//! 测量场景（对应任务要求）：
//! 1. 冷启动：模板编译 / 首次渲染（含首条连接建立）。
//! 2. 热渲染 p50/p95：纯预取上下文 vs 函数 miss 实时查询 vs 请求缓存命中。
//! 3. 并发 32 路渲染饱和：吞吐与排队（许可 16 与 8 对照；许可数在 Bridge 构造时固定，
//!    每个并发档位单独建桥）。
//! 4. 慢查询 200ms 在 100ms deadline 下的失败传播时间分布。

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use template_bridge::pg::{self, QueryFacade};
use template_bridge::{Bridge, Budgets, FailureKind, RenderScope};
use tokio::runtime::Handle;

fn pct(times: &mut Vec<Duration>, q: f64) -> Duration {
    times.sort();
    let idx = (((times.len() - 1) as f64) * q).round() as usize;
    times[idx]
}

fn stats(times: &mut Vec<Duration>) -> String {
    let mean = times.iter().sum::<Duration>() / times.len() as u32;
    format!(
        "p50={:8.3}ms p95={:8.3}ms max={:8.3}ms mean={:8.3}ms n={}",
        pct(times, 0.50).as_secs_f64() * 1000.0,
        pct(times, 0.95).as_secs_f64() * 1000.0,
        times.iter().max().unwrap().as_secs_f64() * 1000.0,
        mean.as_secs_f64() * 1000.0,
        times.len(),
    )
}

fn scope(facade: &QueryFacade, marker: &str, budgets: Budgets) -> RenderScope {
    RenderScope::new(
        Handle::current(),
        facade.clone(),
        marker.to_string(),
        budgets,
    )
}

const TPL_LIST: &str = "list.html";
const TPL_CTX: &str = "ctx.html";

/// 函数 miss：1 次 get_posts(limit=10, offset=off) 实时查询。
const FN_MISS: &str = r#"{% set r = get_posts(limit=10, offset=off) %}{% for p in r.items %}{{ p.slug }};{% endfor %}"#;
/// 函数缓存命中：同参数 5 次（1 miss + 4 命中）。
const FN_HIT5: &str = r#"{{ raw_query("posts:10:0").items[0].slug }}{% for i in range(4) %}{% set _ = raw_query("posts:10:0") %}{% endfor %}"#;
/// 函数 miss × 5：同形状模板 5 个不同查询键。
const FN_MISS5: &str =
    r#"{% for i in range(5) %}{{ raw_query("posts:2:" ~ i).items[0].slug }}{% endfor %}"#;
/// 纯预取上下文。
const CTX_TPL: &str = r#"{% for p in posts %}{{ p.slug }};{% endfor %}"#;

fn templates() -> Vec<(&'static str, &'static str)> {
    vec![
        (TPL_LIST, FN_MISS),
        (TPL_CTX, CTX_TPL),
        ("hit5.html", FN_HIT5),
        ("miss5.html", FN_MISS5),
    ]
}

async fn prefetch_items(facade: &QueryFacade) -> Vec<serde_json::Value> {
    let prefetched = facade("posts:10:0").await.unwrap();
    serde_json::to_value(&prefetched).unwrap()["items"]
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    println!("=== M0 主题桥接原型基准（release） ===");
    println!(
        "环境：{} {} | 逻辑核 {}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0),
    );

    let dsn = pg::recreate_database().await;
    let pool = pg::connect_test(&dsn, 32).await;
    let version: String = sqlx::query_scalar("SELECT version()")
        .fetch_one(&pool)
        .await
        .unwrap();
    println!("数据库：{version}");

    let (facade, facade_queries) = pg::query_facade(pool.clone());
    let default_budgets = Budgets::default();

    // ------------------------------------------------------------------
    // 1. 冷启动：模板编译 vs 首渲染
    // ------------------------------------------------------------------
    println!("\n--- 1. 冷启动 ---");
    let t_compile = Instant::now();
    let cold_bridge = Bridge::new(&[(TPL_LIST, FN_MISS)], 16, Duration::from_secs(5)).unwrap();
    println!(
        "模板编译+建桥（1 个模板）：{:.3}ms",
        t_compile.elapsed().as_secs_f64() * 1000.0
    );

    let t_first = Instant::now();
    let first_out = cold_bridge
        .render(
            scope(&facade, "cold", default_budgets.clone()),
            TPL_LIST,
            serde_json::json!({ "off": 0 }),
        )
        .await
        .expect("首渲染失败");
    println!(
        "首次渲染（含首条 sqlx 连接建立 + 1 次查询，输出 {} 字节）：{:.3}ms",
        first_out.len(),
        t_first.elapsed().as_secs_f64() * 1000.0
    );
    drop(cold_bridge);

    // ------------------------------------------------------------------
    // 2. 热渲染延迟对照（顺序，300 次）
    // ------------------------------------------------------------------
    println!("\n--- 2. 热渲染延迟（顺序 300 次） ---");
    let hot_bridge = Bridge::new(&templates(), 32, Duration::from_secs(10)).unwrap();
    let iters = 300;

    // 2a. 纯预取上下文：预取计时 + 仅渲染计时分开报告。
    let mut t_prefetch = Vec::new();
    let mut t_ctx_render = Vec::new();
    let mut t_ctx_e2e = Vec::new();
    for _ in 0..iters {
        let e2e = Instant::now();
        let t0 = Instant::now();
        let items = prefetch_items(&facade).await;
        t_prefetch.push(t0.elapsed());
        let t1 = Instant::now();
        let out = hot_bridge
            .render(
                scope(&facade, "ctx", default_budgets.clone()),
                TPL_CTX,
                serde_json::json!({ "posts": items }),
            )
            .await
            .unwrap();
        t_ctx_render.push(t1.elapsed());
        t_ctx_e2e.push(e2e.elapsed());
        assert_eq!(out.split(';').count(), 11);
    }
    println!(
        "纯预取·仅查询（application 侧预取 10 行）：  {}",
        stats(&mut t_prefetch)
    );
    println!(
        "纯预取·仅渲染：                             {}",
        stats(&mut t_ctx_render)
    );
    println!(
        "纯预取·端到端（预取+渲染）：                {}",
        stats(&mut t_ctx_e2e)
    );

    // 2b. 函数 miss：每渲染 1 次实时查询（offset 轮转 0..40；请求缓存 per-request，
    //     每渲染新 scope，测的是真实 miss 路径）。
    let mut t_fn_miss = Vec::new();
    for i in 0..iters {
        let t0 = Instant::now();
        let out = hot_bridge
            .render(
                scope(&facade, "miss", default_budgets.clone()),
                TPL_LIST,
                serde_json::json!({ "off": i % 40 }),
            )
            .await
            .unwrap();
        t_fn_miss.push(t0.elapsed());
        assert_eq!(out.split(';').count(), 11);
    }
    println!(
        "函数 miss·单查询（端到端）：                {}",
        stats(&mut t_fn_miss)
    );

    // 2c. 同形状对照：5 次调用全 miss vs 1 miss + 4 缓存命中。
    let mut t_miss5 = Vec::new();
    let mut t_hit5 = Vec::new();
    for _ in 0..iters {
        let t0 = Instant::now();
        hot_bridge
            .render(
                scope(&facade, "m5", default_budgets.clone()),
                "miss5.html",
                serde_json::json!({}),
            )
            .await
            .unwrap();
        t_miss5.push(t0.elapsed());
        let t0 = Instant::now();
        let out = hot_bridge
            .render(
                scope(&facade, "h5", default_budgets.clone()),
                "hit5.html",
                serde_json::json!({}),
            )
            .await
            .unwrap();
        t_hit5.push(t0.elapsed());
        assert!(out.starts_with("post-00"));
    }
    println!(
        "函数 5 次全 miss（端到端）：                {}",
        stats(&mut t_miss5)
    );
    println!(
        "函数 1 miss + 4 命中（端到端）：            {}",
        stats(&mut t_hit5)
    );

    // 2d. 参考值：noop 渲染（scope 构造 + env 副本 + 许可 + 调度，无查询无输出）。
    let micro_bridge = Bridge::new(&[("noop.html", "ok")], 32, Duration::from_secs(10)).unwrap();
    let mut t_noop = Vec::new();
    for _ in 0..2000 {
        let t0 = Instant::now();
        let out = micro_bridge
            .render(
                scope(&facade, "noop", default_budgets.clone()),
                "noop.html",
                serde_json::json!({}),
            )
            .await
            .unwrap();
        t_noop.push(t0.elapsed());
        assert_eq!(out, "ok");
    }
    println!(
        "noop 渲染（请求侧固定开销参考）：           {}",
        stats(&mut t_noop)
    );

    // ------------------------------------------------------------------
    // 3. 并发 32 路饱和（每 worker 25 次渲染 = 800 次）
    // ------------------------------------------------------------------
    println!("\n--- 3. 并发 32 路饱和（32 worker × 25 渲染） ---");
    // 预热连接池：排除首次并发建连风暴对饱和场景的干扰（建连成本已计入冷启动场景）。
    {
        let mut warm = Vec::new();
        for _ in 0..32 {
            let p = pool.clone();
            warm.push(tokio::spawn(async move {
                sqlx::query("SELECT 1").execute(&p).await.unwrap();
            }));
        }
        for w in warm {
            w.await.unwrap();
        }
        println!("连接池预热完成（32 连接）");
    }

    // 3a. 纯预取上下文（worker 内预取，端到端含查询），许可 16。
    {
        let bridge = Arc::new(Bridge::new(&templates(), 16, Duration::from_secs(10)).unwrap());
        let mut times = run_workers(&bridge, &facade, &default_budgets, |i, facade| async move {
            let items = prefetch_items(&facade).await;
            (
                TPL_CTX.to_string(),
                serde_json::json!({ "posts": items, "off": i }),
            )
        })
        .await;
        println!(
            "纯预取·端到端（许可 16）：                 {}",
            stats(&mut times)
        );
    }

    // 3b. 函数 miss 实时查询，许可 16。
    {
        let bridge = Arc::new(Bridge::new(&templates(), 16, Duration::from_secs(10)).unwrap());
        let mut times = run_workers(&bridge, &facade, &default_budgets, |i, _facade| async move {
            (TPL_LIST.to_string(), serde_json::json!({ "off": i % 40 }))
        })
        .await;
        println!(
            "函数 miss·单查询（许可 16）：              {}",
            stats(&mut times)
        );
    }

    // 3c. 函数 miss 实时查询，许可 8（排队对照：32 并发 > 8 许可）。
    {
        let bridge = Arc::new(Bridge::new(&templates(), 8, Duration::from_secs(10)).unwrap());
        let mut times = run_workers(&bridge, &facade, &default_budgets, |i, _facade| async move {
            (TPL_LIST.to_string(), serde_json::json!({ "off": i % 40 }))
        })
        .await;
        println!(
            "函数 miss·单查询（许可 8，排队）：         {}",
            stats(&mut times)
        );
    }

    // ------------------------------------------------------------------
    // 4. 慢查询失败传播：200ms 查询 vs 100ms deadline
    // ------------------------------------------------------------------
    println!("\n--- 4. 慢查询失败传播（sleep 200ms，deadline 100ms，50 次） ---");
    let fail_bridge = Bridge::new(
        &[("slow.html", r#"{{ raw_query("sleep:200") }}"#)],
        8,
        Duration::from_secs(5),
    )
    .unwrap();
    let budgets = Budgets {
        deadline: Duration::from_millis(100),
        fuel: Some(10_000_000),
        ..default_budgets.clone()
    };
    let mut t_fail = Vec::new();
    for _ in 0..50 {
        let t0 = Instant::now();
        let err = fail_bridge
            .render(
                scope(&facade, "slow", budgets.clone()),
                "slow.html",
                serde_json::json!({}),
            )
            .await
            .err()
            .expect("慢查询应失败");
        assert_eq!(err.kind, FailureKind::QueryTimeout);
        t_fail.push(t0.elapsed());
    }
    println!("失败传播耗时：{}", stats(&mut t_fail));
    println!(
        "门面查询总数（观测）：{}",
        facade_queries.load(Ordering::Relaxed)
    );
}

/// 32 个 worker 各渲染 25 次；每渲染端到端计时（含 ctx 准备）。
/// `make` 在计时内执行：预取场景的查询也计入，保证口径一致。
async fn run_workers<M, F>(
    bridge: &Arc<Bridge>,
    facade: &QueryFacade,
    budgets: &Budgets,
    make: M,
) -> Vec<Duration>
where
    M: Fn(usize, QueryFacade) -> F + Send + Sync + Clone + 'static,
    F: Future<Output = (String, serde_json::Value)> + Send,
{
    let workers = 32;
    let iters = 25;
    let make = Arc::new(make);
    let mut handles = Vec::new();
    for w in 0..workers {
        let bridge = bridge.clone();
        let facade = facade.clone();
        let budgets = budgets.clone();
        let make = make.clone();
        handles.push(tokio::spawn(async move {
            let mut times = Vec::with_capacity(iters);
            for i in 0..iters {
                let seq = w * iters + i;
                let t0 = Instant::now();
                let (tpl, ctx) = make(seq, facade.clone()).await;
                let out = bridge
                    .render(scope(&facade, "sat", budgets.clone()), &tpl, ctx)
                    .await
                    .expect("饱和渲染不应失败");
                times.push(t0.elapsed());
                assert!(!out.is_empty());
            }
            times
        }));
    }
    let mut all = Vec::with_capacity(workers * iters);
    let wall = Instant::now();
    for h in handles {
        all.extend(h.await.expect("worker 无 panic"));
    }
    let wall = wall.elapsed();
    println!(
        "  [吞吐] {} 渲染 / {:.2}s = {:.0} 渲染/s",
        all.len(),
        wall.as_secs_f64(),
        all.len() as f64 / wall.as_secs_f64()
    );
    all
}
