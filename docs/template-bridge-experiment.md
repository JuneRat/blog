# 模板同步/异步桥接实验记录（归档）

2026-09 的独立原型验证了 MiniJinja 同步模板函数调用异步查询的可行性。
正式实现现已位于 [渲染运行时](../crates/infrastructure/src/rendering.rs)、[受控模板函数](../crates/infrastructure/src/theme_functions.rs)和[查询预算](../crates/application/src/theme_data.rs)。独立实验工程已从当前源码树移除，原始源码可从 Git 历史查阅。

本页保留当时的环境、测量和失败场景，作为 [ADR-0002](adr/0002-template-data-functions.md) 的证据。当前接口、预算和验收入口以[主题与渲染](themes-and-rendering.md)及工作区测试为准；以下吞吐不代表现有部署性能。跨请求页面缓存仍属于 [ADR-0004](adr/0004-public-cache-generation.md) 的独立提议。

## 历史测量环境

| 项 | 值 |
|---|---|
| OS | macOS 27.0（Darwin 27.0.0，arm64） |
| CPU | Apple M4，10 逻辑核 |
| Rust | rustc 1.98.1 / cargo 1.98.1 |
| PostgreSQL | 18.6（Docker `postgres:18-alpine`，loopback 127.0.0.1:5432） |
| minijinja | **2.24.0**（显式开启 `fuel` 特性；默认特性含 `serde`） |
| sqlx | 0.8.6（runtime-tokio、postgres、time、uuid） |
| tokio | 1.53.1（rt-multi-thread、macros、sync、time） |
| uuid / thiserror / serde_json | 1.26.1 / 2 / 1 |

## 历史测量结果（release，真实 PostgreSQL）

同一页面 = 10 篇已发布文章的 slug 列表。顺序场景 n=300；饱和 32 worker × 25 渲染。
三次完整运行取范围；饱和场景连接池已预热（未预热时首波并发建连出现 ~200ms 尖峰）。

### 冷启动

| 项 | 数值 |
|---|---|
| 模板编译 + 建桥（1 个模板） | 0.35–0.43 ms |
| 首次渲染（含首条 sqlx 建连 + 1 次查询，输出 80B） | 2.3–3.2 ms |

### 顺序热渲染延迟

| 场景 | p50 | p95 | 说明 |
|---|---|---|---|
| 纯预取·仅查询（application 侧预取 10 行） | 0.24–0.49 ms | 0.29–0.71 ms | 本地查询 RT |
| 纯预取·仅渲染 | 0.015–0.033 ms | 0.03–0.05 ms | |
| 纯预取·端到端（预取+渲染） | 0.26–0.53 ms | 0.33–0.75 ms | |
| **函数 miss·单查询（端到端）** | **0.22–0.24 ms** | **0.28–0.30 ms** | 含桥接全路径 |
| 函数 5 次全 miss（端到端） | 0.92–1.10 ms | 1.06–1.33 ms | 渲染线程串行 5×RT |
| 函数 1 miss + 4 缓存命中（端到端） | 0.20–0.23 ms | 0.25–0.29 ms | 命中近乎免费 |
| noop 渲染（请求侧固定开销参考） | 0.005 ms | 0.007 ms | scope+env 副本+许可+调度 |

**读法**：顺序场景下"函数 miss 实时查询"与"纯预取端到端"同量级——两者都被一次
~0.2–0.5ms 的本地查询主导，桥接本身开销 µs 级。同参数第 2 次起命中请求缓存，
把 5 次调用模板从 ~0.9ms 拉回 ~0.2ms；5 个不同查询则严格串行（占用渲染线程）。

### 并发 32 路饱和（每 worker 25 次渲染，n=800）

| 场景 | 吞吐 | p50 | p95 | max |
|---|---|---|---|---|
| 纯预取·端到端（许可 16） | 16.0k 渲染/s | 1.66 ms | 2.98 ms | 9.9 ms |
| 函数 miss·单查询（许可 16） | 16.4k 渲染/s | 1.88 ms | 2.41 ms | 3.8 ms |
| 函数 miss·单查询（许可 8，排队） | 12.7k 渲染/s | 2.49 ms | 2.91 ms | 3.5 ms |

**读法**：许可 16 时函数取数与纯预取吞吐相当（差异在运行间噪声内）；许可减半后
吞吐 -23%、p50 +33%，排队线性可见且无失控。饱和行为受 sqlx 池（32 连接）与
spawn_blocking 线程池共同影响，未发生 PermitWaitTimeout（permit_wait 10s 内全部消化）；
有限等待失败路径由测试 `permit_saturation_fails_after_bounded_wait` 单独验证（80ms 等待上限）。

### 慢查询失败传播（pg_sleep 200ms，deadline 100ms，n=50）

| p50 | p95 | max |
|---|---|---|
| 101.9 ms | 102.7 ms | 102.8 ms |

失败在 deadline + ~2ms 内受控返回（`QueryTimeout`），不等待查询自然结束（200ms），
也不挂住调用方；随后阻塞线程自行退出、许可归还（测试观测）。

## 历史失败场景矩阵

以下为当时独立原型的测试结果（17 项测试通过），用于记录决策证据，不代表当前实现的验收结果。

| 场景 | 实际行为 | 符合预期 |
|---|---|---|
| 慢查询 pg_sleep 500ms vs deadline 100ms | `QueryTimeout` 受控错误，<300ms 返回（≈deadline），阻塞线程随后退出 | ✅ |
| 廉价调用高频循环跨过 deadline | 下一次宿主调用入口 `DeadlineExceeded`，循环停止 | ✅ |
| 查询报错（SELECT 1/0） | `DbError`，消息含 "division by zero"，无 panic | ✅ |
| fuel 耗尽（百万次空循环） | `OutOfFuel`，立即失败 | ✅ |
| fuel=50 下 300ms 慢查询 | **照常完成**（fuel 不计宿主 I/O），deadline 单独限制 | ✅（边界如实记录） |
| 自包含 include 递归（limit 50） | `RecursionLimit`（source 链分类），无栈溢出 | ✅ |
| 5MB 单值输出（fuel=1000） | 完整渲染——**无原生输出大小上限** | ⚠️ 需宿主侧补限 |
| 循环内 10 个唯一查询、预算 3 | 恰在第 4 次独立查询前 `QueryBudgetExhausted`，fn_name 记录 | ✅ |
| 同参数 5 连调用、查询预算 1 | 成功（1 miss + 4 命中，不重复计查询） | ✅ |
| 调用预算 3、4 次调用 | 第 4 次 `CallBudgetExhausted`（即使会命中缓存） | ✅ |
| 许可 2 被慢渲染占用，第 3 路 | `PermitWaitTimeout`，在 80ms 有限等待内返回 | ✅ |
| 客户端 40ms 放弃（deadline 120ms） | 许可不提前释放；deadline 后线程退出、许可归还；后续渲染正常 | ✅ |
| 32 路并行、每请求随机标记 | 输出零串用；全局查询数恰 32（每请求缓存去重生效） | ✅ |
| env 副本 add_function | 副本间、与基础环境互不影响 | ✅ |
| 未知关键字参数 get_posts(bogus=3) | `TooManyArguments` → 受控 `TemplateError` | ✅ |
| 函数取数 vs 预取输出一致性 | 两条路径输出逐字节一致 | ✅ |

## 采纳结论

**可行。** 在多线程 Tokio runtime 上，`spawn_blocking` 渲染 + 函数内
`Handle::block_on(timeout(剩余deadline, 查询))` 的桥接稳定工作：17 项真实库测试全绿，
全部失败路径受控、无 panic、无阻塞线程泄漏；顺序延迟与纯预取同量级（都被单次本地
查询主导），饱和吞吐与纯预取相当（16.4k vs 16.0k 渲染/s @ 许可16），失败传播在
deadline+2ms 内。请求隔离经 per-request env 副本 + 闭包绑定实现，固定开销 µs 级。

**采纳条件**：

1. 必须是多线程 runtime；桥接只发生在 spawn_blocking 工作线程内。
2. minijinja 显式开启 `fuel` 特性；宿主 I/O 一律用剩余 deadline 包裹 timeout——fuel
   只管模板指令，不管 I/O。
3. 输出大小上限需宿主侧自行实现（引擎没有）。
4. 渲染许可移入阻塞任务持有到退出；饱和有限等待用 `timeout(acquire_owned())`。
5. 每请求 env 副本 + 闭包捕获 RenderScope；绝不共享带请求状态的全局函数表。
