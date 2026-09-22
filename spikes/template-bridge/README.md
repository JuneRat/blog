# M0 主题桥接原型报告：spikes/template-bridge

状态：**原型完成，方案可行**。17 项集成测试全绿（真实 PostgreSQL）；测量与失败场景见下文。
本工程通过 Cargo.toml 中的空 `[workspace]` 表脱离仓库根 workspace，生产 crate 不得依赖本工程。

## 1. 目的

在冻结正式主题模板函数 API 之前，验证 docs/themes-and-rendering.md §4 提出的核心方案：

1. **同步-异步桥接**：MiniJinja 模板函数是同步接口；验证从 `spawn_blocking` 工作线程内
   通过 `tokio::runtime::Handle::block_on` 驱动带超时的 sqlx 查询 future 是否可行
   （绝不新建 runtime、绝不在异步执行线程上 block_on）。
2. **预算与截止时间**：fuel、递归深度、渲染 deadline（传播到每次宿主查询）、
   每页数据库查询次数预算（默认 10）与函数调用预算；饱和时有限等待后受控失败。
3. **请求隔离**：请求级 RenderScope（deadline、预算、请求缓存、随机标记）经
   per-request `Environment` 副本 + 闭包绑定，并行渲染无状态串用。
4. **失败场景**：慢查询、deadline 超时、查询报错、fuel 耗尽、递归过深、预算耗尽——
   全部受控错误、不 panic、不泄漏阻塞线程（spawn_blocking 不可 abort，须验证线程确实退出）。
5. **性能对照**：纯预取上下文渲染 vs 函数 miss 实时查询渲染的冷/热延迟与并发饱和吞吐。

## 2. 环境

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

版本与根 Cargo.toml 的 workspace.dependencies 对齐（minijinja 2、sqlx 0.8、tokio 1、uuid 1）。
数据库经 `BLOG_TEST_ADMIN_URL`（默认 `postgres://blog:blog@127.0.0.1:5432/postgres`）推导
同主机测试库 `spike_bridge_test`（替换最后路径段），非 loopback 主机直接 panic 拒绝
（守卫照抄 crates/server/tests/common/mod.rs）。建表用 raw SQL
（`spike_posts(id uuid pk, slug text, title text, published bool)` + 50 行种子），不使用生产迁移目录。

## 3. 设计要点

### 3.1 桥接机制（src/lib.rs）

```
异步侧（use case）                            阻塞池（渲染）
─────────────────────────────                ─────────────────────────────
有限等待获取渲染许可                            per-request env = base.clone()
  timeout(permit_wait,                          + register_request_functions(env, Arc<ScopeState>)
    permits.acquire_owned())                    + set_fuel / set_recursion_limit
spawn_blocking(move || {                       env.get_template(tpl).render(ctx)
  许可与占用守卫移入闭包，持有到退出                 │
  … 渲染 …                                        ├─ 函数闭包 → ScopeState::host_call
})                                                   │ 1) 函数调用预算（命中也计）
await JoinHandle（客户端可放弃，                    │ 2) 剩余 deadline 检查
  许可不提前释放）                                     │ 3) 请求缓存命中 → 返回
                                                     │ 4) 查询预算（仅 miss 计）
                                                     │ 5) handle.block_on(
                                                     │      timeout(剩余deadline, 查询future))
                                                     │ 6) 结果写入请求缓存
```

- 只从 `spawn_blocking` 工作线程调用 `Handle::block_on`；不新建 runtime，不在异步执行线程 block_on。
- 每次宿主查询的超时 = **剩余 deadline**（不是固定值），保证总渲染时长被 deadline 覆盖。
- 许可（`OwnedSemaphorePermit`）与阻塞线程占用守卫都移入阻塞闭包，**持有到渲染退出**；
  客户端提前放弃 JoinHandle 不会提前释放许可（测试验证）。
- 饱和有限等待：`tokio::time::timeout(permit_wait, acquire_owned())`——tokio 1.53 已移除
  `Semaphore::acquire_timeout`，此为等价写法（超时取消不丢许可）。

### 3.2 预算清单（Budgets，默认值）

| 预算 | 默认 | 语义 |
|---|---|---|
| `deadline` | 500ms | 渲染截止时间；入口检查 + 每次宿主查询的 timeout |
| `query_budget` | 10 | 每页**独立**数据库查询次数；同参数命中请求缓存不重复计 |
| `call_budget` | 64 | 模板函数调用总次数；**缓存命中也计入** |
| `fuel` | Some(200_000) | MiniJinja VM 指令预算（需 `fuel` 特性；不限制宿主 I/O，见 §6） |
| `recursion_limit` | 100 | 递归深度（minijinja 无 stacker 特性时硬上限 500） |
| 渲染许可 | 16（基准配置） | 并发渲染上限；饱和有限等待默认 250ms 后 PermitWaitTimeout |

失败分类（`FailureKind`）：`DeadlineExceeded` / `QueryTimeout` / `QueryBudgetExhausted` /
`CallBudgetExhausted` / `DbError` / `PermitWaitTimeout` / `OutOfFuel` / `RecursionLimit` /
`TemplateError`。宿主错误经 scope 暂存精确分类，minijinja 错误按 kind + source 链归类。

### 3.3 请求隔离机制

- 每请求构造 `RenderScope`（随机 uuid 标记 + 独立预算原子计数 + 独立请求缓存 HashMap），
  经 `Arc` 捕获进函数闭包，注册在 **per-request `Environment` 副本**上。
- `Environment::clone` 共享已编译模板（`Arc<CompiledTemplate>`），filters/globals 是
  `Arc<BTreeMap>` + `Arc::make_mut` **写时复制**：副本上 `add_function` 不影响基础环境或
  兄弟副本（测试 `env_clone_add_function_is_isolated` 验证）。
- 32 路并行渲染各注入独立标记：输出互不串用、每请求缓存各自去重（测试
  `parallel_requests_no_state_crossing`）。
- noop 渲染（scope 构造 + env 副本 + 许可 + spawn_blocking 往返，零查询零输出）
  p50 ≈ **5µs**：per-request 副本方案的开销可忽略。

## 4. 测量结果（release，真实 PostgreSQL）

同一页面 = 10 篇已发布文章的 slug 列表。顺序场景 n=300；饱和 32 worker × 25 渲染。
三次完整运行取范围；饱和场景连接池已预热（未预热时首波并发建连出现 ~200ms 尖峰）。

### 4.1 冷启动

| 项 | 数值 |
|---|---|
| 模板编译 + 建桥（1 个模板） | 0.35–0.43 ms |
| 首次渲染（含首条 sqlx 建连 + 1 次查询，输出 80B） | 2.3–3.2 ms |

### 4.2 顺序热渲染延迟

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

### 4.3 并发 32 路饱和（每 worker 25 次渲染，n=800）

| 场景 | 吞吐 | p50 | p95 | max |
|---|---|---|---|---|
| 纯预取·端到端（许可 16） | 16.0k 渲染/s | 1.66 ms | 2.98 ms | 9.9 ms |
| 函数 miss·单查询（许可 16） | 16.4k 渲染/s | 1.88 ms | 2.41 ms | 3.8 ms |
| 函数 miss·单查询（许可 8，排队） | 12.7k 渲染/s | 2.49 ms | 2.91 ms | 3.5 ms |

**读法**：许可 16 时函数取数与纯预取吞吐相当（差异在运行间噪声内）；许可减半后
吞吐 -23%、p50 +33%，排队线性可见且无失控。饱和行为受 sqlx 池（32 连接）与
spawn_blocking 线程池共同影响，未发生 PermitWaitTimeout（permit_wait 10s 内全部消化）；
有限等待失败路径由测试 `permit_saturation_fails_after_bounded_wait` 单独验证（80ms 等待上限）。

### 4.4 慢查询失败传播（pg_sleep 200ms，deadline 100ms，n=50）

| p50 | p95 | max |
|---|---|---|
| 101.9 ms | 102.7 ms | 102.8 ms |

失败在 deadline + ~2ms 内受控返回（`QueryTimeout`），不等待查询自然结束（200ms），
也不挂住调用方；随后阻塞线程自行退出、许可归还（测试观测）。

## 5. 失败场景矩阵

全部场景来自 `tests/bridge.rs`（17 项测试，全绿；"预期"来自 themes-and-rendering.md §4/§7）。

| 场景 | 实际行为 | 符合预期 |
|---|---|---|
| 慢查询 pg_sleep 500ms vs deadline 100ms | `QueryTimeout` 受控错误，<300ms 返回（≈deadline），阻塞线程随后退出 | ✅ |
| 廉价调用高频循环跨过 deadline | 下一次宿主调用入口 `DeadlineExceeded`，循环停止 | ✅ |
| 查询报错（SELECT 1/0） | `DbError`，消息含 "division by zero"，无 panic | ✅ |
| fuel 耗尽（百万次空循环） | `OutOfFuel`，立即失败 | ✅ |
| fuel=50 下 300ms 慢查询 | **照常完成**（fuel 不计宿主 I/O，见 §6.1），deadline 单独限制 | ✅（边界如实记录） |
| 自包含 include 递归（limit 50） | `RecursionLimit`（source 链分类），无栈溢出 | ✅ |
| 5MB 单值输出（fuel=1000） | 完整渲染——**无原生输出大小上限**（见 §6.2） | ⚠️ 需宿主侧补限 |
| 循环内 10 个唯一查询、预算 3 | 恰在第 4 次独立查询前 `QueryBudgetExhausted`，fn_name 记录 | ✅ |
| 同参数 5 连调用、查询预算 1 | 成功（1 miss + 4 命中，不重复计查询） | ✅ |
| 调用预算 3、4 次调用 | 第 4 次 `CallBudgetExhausted`（即使会命中缓存） | ✅ |
| 许可 2 被慢渲染占用，第 3 路 | `PermitWaitTimeout`，在 80ms 有限等待内返回 | ✅ |
| 客户端 40ms 放弃（deadline 120ms） | 许可不提前释放；deadline 后线程退出、许可归还；后续渲染正常 | ✅ |
| 32 路并行、每请求随机标记 | 输出零串用；全局查询数恰 32（每请求缓存去重生效） | ✅ |
| env 副本 add_function | 副本间、与基础环境互不影响 | ✅ |
| 未知关键字参数 get_posts(bogus=3) | `TooManyArguments` → 受控 `TemplateError` | ✅ |
| 函数取数 vs 预取输出一致性 | 两条路径输出逐字节一致 | ✅ |

## 6. MiniJinja fuel/API 能力边界（如实记录）

1. **fuel 非默认特性**：`set_fuel`/`ErrorKind::OutOfFuel` 需 `features = ["fuel"]`。
   fuel 按 **VM 指令**计量（源码 `FuelTracker::track(instr)`，每条指令 0 或 1），
   **完全不计量宿主函数内部 I/O**：fuel=50 时 300ms 数据库睡眠照常执行完（测试
   `fuel_does_not_limit_host_io`）。宿主 I/O 必须单独用 deadline/timeout 限制——
   本原型把剩余 deadline 包在每次查询外，已覆盖。
2. **无原生输出大小限制**：fuel=1000 时 5MB 单值输出完整渲染（输出一个 5MB 字符串
   只消耗个位数指令）。若需要输出上限，须宿主侧实现（如渲染后长度校验+截断，或包装
   formatter），不能依赖 fuel。
3. **递归限制**：默认 500；未启用 `stacker` 特性时 `set_recursion_limit(n>500)` 被
   `min(500)` **静默钳制**。超限错误是 `ErrorKind::InvalidOperation("recursion limit
   exceeded")`；include 嵌套时内层错误被包进外层错误的 source 链，分类需遍历
   `std::error::Error::source()`（本原型已实现）。
4. **内建防护**：`range` 生成器上限 100000 元素（"range has too many elements"），
   与 fuel 无关的独立护栏。
5. **函数参数**：闭包参数按位置绑定；主题风格的关键字参数需 `Kwargs` 类型 +
   `assert_all_used()` 拒绝未知参数（本原型 `get_posts(limit=…, offset=…)` 即此写法）。
6. **Environment 副本语义**：clone 共享已编译模板（Arc），函数表写时复制（§3.3）；
   每请求副本固定开销 ≈ 5µs（noop 渲染 p50）。生产 rendering.rs 的 Box::leak 模板源
   方式与副本方案兼容。
7. **模板语言注意**：Jinja 的 for 循环内 `{% set %}` 不外溢到循环外（作用域规则，
   非引擎缺陷，主题作者需知）。
8. **tokio 1.53 变化**：`Semaphore::acquire_timeout` 已移除；`timeout(d, acquire_owned())`
   等价。`OwnedSemaphorePermit` 可移入 spawn_blocking 闭包持有到退出。
9. **sqlx 连接池绑定 runtime**：池必须在驱动它的 runtime 上创建。跨 runtime 共享池
   （如 `#[tokio::test]` 每测一个 runtime 共享 OnceCell 池）会让连接在旧 runtime 销毁后
   永久挂起——本原型测试改为每测自建池。生产单 runtime 无此问题，但值得写入装配注意事项。
10. **spawn_blocking 不可 abort**：仅靠外层 future timeout 不足以释放线程；本原型把
    剩余 deadline 传播进每次宿主调用，实测客户端放弃后线程在 deadline 处自行退出
    （占用计数归零、许可归还）。

## 7. 结论

### 7.1 方案可行性

**可行。** 在多线程 Tokio runtime 上，`spawn_blocking` 渲染 + 函数内
`Handle::block_on(timeout(剩余deadline, 查询))` 的桥接稳定工作：17 项真实库测试全绿，
全部失败路径受控、无 panic、无阻塞线程泄漏；顺序延迟与纯预取同量级（都被单次本地
查询主导），饱和吞吐与纯预取相当（16.4k vs 16.0k 渲染/s @ 许可16），失败传播在
deadline+2ms 内。请求隔离经 per-request env 副本 + 闭包绑定实现，固定开销 µs 级。

**前提条件**（写入正式实现的硬约束）：

1. 必须是多线程 runtime；桥接只发生在 spawn_blocking 工作线程内。
2. minijinja 显式开启 `fuel` 特性；宿主 I/O 一律用剩余 deadline 包裹 timeout——fuel
   只管模板指令，不管 I/O。
3. 输出大小上限需宿主侧自行实现（引擎没有）。
4. 渲染许可移入阻塞任务持有到退出；饱和有限等待用 `timeout(acquire_owned())`。
5. 每请求 env 副本 + 闭包捕获 RenderScope；绝不共享带请求状态的全局函数表。

### 7.2 对正式主题函数 API 的约束建议

1. **RenderScope 由后端构造、不可变**：deadline、预算、读取范围、请求缓存只进闭包；
   模板参数与模板变量不得影响授权与预算（不做可覆盖变量的授权依据）。
2. **函数签名形态**：`fn name(Kwargs) -> Result<Value, Error>`；位置闭包参数不适配
   主题关键字参数风格；未知参数必须 `assert_all_used()` 拒绝。
3. **预算默认值**（本机实测校准的初值，供压测再调）：每页查询 10 次、函数调用 64 次、
   fuel 200k、递归 100、deadline 500ms、许可等待 250ms、并发渲染许可 16。
   命中请求缓存计入调用预算、不计入查询预算——语义按 docs §4 落地。
4. **热点路径必须走请求缓存**：同参数重复调用近乎免费（~µs），miss 则占用渲染线程
   串行查询（5 连 miss ≈ 5×RT）。路由已知数据应继续预取；模板函数服务侧栏等补充数据。
5. **错误契约**：函数错误统一映射为带类别（预算/截止/DB/模板）的受控 5xx 原语，
   记录函数名 + 模板位置；minijinja 错误分类需遍历 source 链。

### 7.3 遗留问题

1. 输出大小限制未实现（建议：渲染结果长度阈值 + 超限受控错误，或自定义 formatter 截断）。
2. 数据库语句级超时与 deadline 传播的叠加策略未细测（当前仅 deadline timeout 包裹
   future；drop 后 sqlx 以取消协议归还连接，长事务下的行为未验证）。
3. 饱和 p95 在 2.4–3.0ms 间抖动，与 runtime 调度/sqlx 池参数相关，未做调优；
   loopback 单机测量，网络 RTT 会放大 miss 与预取差距的绝对值。
4. 跨请求读缓存与 generation（ADR-0004 主题）不在本原型范围；请求缓存已验证，
   页面缓存版本校验待 M3 前另行验证。
5. fuel 与调用预算的精确指令成本未逐条标定（仅总量验证）；如需按模板复杂度计费可再测。

## 8. 建议的 ADR-0002/0004 措辞更新（供主代理审阅后套用，本原型未改动 docs/）

**ADR-0002 补充段（建议）：**

> M0 原型（spikes/template-bridge）已验证桥接方案可行：多线程 Tokio runtime 上，
> 同步渲染经 spawn_blocking 进入阻塞池，模板函数在阻塞线程内以
> `Handle::block_on(timeout(剩余截止时间, 查询))` 驱动 sqlx 查询；不新建 runtime，
> 不在异步执行线程 block_on。预算组合为：渲染截止时间（传播到每次宿主调用）、
> 每页独立查询次数（默认 10）、函数调用次数（缓存命中也计入）、fuel 与递归深度。
> fuel 需显式启用 minijinja `fuel` 特性，且只按 VM 指令计量、不限制宿主函数内部 I/O，
> 后者必须由截止时间单独限制；输出大小无引擎原生上限，需宿主侧实现。渲染许可由
> 阻塞任务持有到退出，客户端放弃不提前释放；饱和时有限等待后返回受控错误。
> 请求隔离采用 per-request Environment 副本（模板共享、函数表写时复制，开销微秒级）
> 加闭包捕获不可变 RenderScope；实测并行渲染无状态串用，函数 miss 与纯预取吞吐相当。
> 原型结论：冻结正式主题函数 API 时，采纳"预算化函数取数 + 预取优先"的组合，
> 函数错误统一为带类别的受控失败，不做无限排队或静默降级。

**ADR-0004 补充段（建议）：**

> M0 桥接原型确认请求级查询缓存命中成本可忽略（同参数重复调用不重复计查询预算，
> 仅计入函数调用预算），跨请求页面/读缓存的 generation 版本校验不受桥接机制影响，
> 仍按本 ADR 的方案在启用页面缓存前单独验证命中率、失效与撤回语义。

## 9. 运行方式

```bash
# 前置：loopback PostgreSQL，BLOG_TEST_ADMIN_URL 可用（默认 postgres://blog:blog@127.0.0.1:5432/postgres）
cd spikes/template-bridge
cargo test            # 17 项集成测试（破坏性重建 spike_bridge_test 库）
cargo run --release   # 计时基准（冷/热/饱和/失败传播，§4 数据来源）
```

文件清单：

| 文件 | 内容 |
|---|---|
| `Cargo.toml` | 空 `[workspace]` 脱离根 workspace；依赖版本对齐根配置 |
| `src/lib.rs` | 桥接核心：RenderScope、预算守卫、host_call、函数注册、渲染许可、失败分类 |
| `src/pg.rs` | 测试库装配（loopback 守卫、raw SQL 建表播种）、sqlx 查询门面 |
| `src/main.rs` | 计时基准（release） |
| `tests/bridge.rs` | 17 项集成测试（§5 矩阵来源） |
