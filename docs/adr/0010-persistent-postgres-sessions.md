# ADR-0010：会话持久化到 PostgreSQL

记录日期：2026-09-23。

状态：已采纳并实现。来源：用户要求保留现有认证流程，自行实现 PostgreSQL `SessionStore` 适配器，让服务重启后仍保持登录，同时保留既有授权语义。细化契约见 [会话与失败限流](../identity-and-admin.md#6-会话与失败限流)，恢复影响见 [备份与恢复](../operations-and-recovery.md)。

## 背景

[ADR-0008](0008-thirteen-table-blog-core.md) 的 13 表基线把本站会话与 OAuth 尝试放在单实例有界内存存储，明确「重启全部失效、不承诺不登出」。这在首版是可接受的取舍，但对长期运行站点意味着每次部署/重启都强制全体重新登录。

用户要求在不重写登录系统的前提下实现「重启后仍登录」：应用层已有 `SessionStore` 端口（`create`/`validate`/`revoke`/`revoke_all_for_user`，见 [identity 端口](../../crates/application/src/ports/identity.rs)），只需替换装配处的内存实现。

`tower-sessions` 等成熟 Axum/Tower 会话中间件也提供 PostgreSQL 存储，但采用它意味着改用其 Session 提取器与键值模型，再把本项目已有的按用户批量撤销、`users.version` 即时失效、CSRF 与既有 Cookie 行为接回去；收益主要是通用会话中间件，而这些部分本项目已经具备。因此选择自研最小适配器。

## 决策

1. **新增 `sessions` 表（第 16 张），只替换存储，不改认证流程**。`PostgresSessionStore` 与 `InMemorySessionStore` 实现同一端口契约；装配处替换即可，测试仍可用内存实现。
2. **只存令牌摘要，不存明文**。`token_hash` = SHA-256(令牌) 小写 hex，作主键；明文只在签发时返回一次。cookie 泄露不能从库中反查令牌，库泄露也不能直接当 cookie 使用。
3. **字段覆盖校验所需的一切**：`user_id`（外键）、`csrf_token`、签发时 `user_version`、`created_at`、`last_seen_at`、`expires_at`。CSRF token 对同源 JS 可见、不是秘密，故存明文。
4. **过期语义与内存实现逐字对齐**：空闲过期看 `last_seen_at`（`last_seen_at >= now - idle` 才有效），绝对过期看签发时固定的 `expires_at`（`created_at + absolute`）；每次校验刷新 `last_seen_at`。两个实现使用同一套比较（含 `>=` 边界），避免迁移后行为漂移。
5. **索引服务两个真实查询**：`sessions_by_user(user_id)` 供按用户批量撤销；`sessions_by_expires(expires_at)` 与 `sessions_by_last_seen(last_seen_at)` 供过期清理与容量淘汰。
6. **容量上限与批量撤销共用事务级 advisory lock**：`create` 的「清理过期—按需淘汰—插入」与 `revoke_all_for_user` 的整用户删除在同一把锁内串行。前者保证并发创建精确不超 `max_entries`；后者保证撤销返回后不会再有「此前已开始、尚未提交」的创建落库（排在撤销之后的创建属于撤销后的新登录，允许存活）。这与内存实现单锁下的语义一致。
7. **保留 `users.version` 绑定**：会话仍记录签发版本，校验时与应用层重读的版本比对。这既是跨进程撤权的既有保障，也让「存储是否共享」不成为正确性前提。
8. **时钟由构造注入**：生产用系统时钟，测试用可推进的逻辑时钟，过期断言不需要 sleep。
9. **OAuth 尝试仍留在内存**：一次性 state 随进程重启作废是有意行为，且不参与「重启后仍登录」。

## 被否方案

- **引入 `tower-sessions`**：其价值在通用会话中间件与提取器；本项目已有 Cookie/CSRF/版本失效/批量撤销语义，迁入后要逐项接回，净收益为负，还多一层与既有授权语义的耦合。
- **Redis 或独立会话服务**：为单实例站点再引入一个必须有状态组件；PostgreSQL 已在依赖内，且撤权路径本来就读主库。
- **把会话塞进 `settings` 或用签名 cookie 自包含**：前者隐藏模型，后者无法服务端即时撤销（改密/撤权后旧 cookie 仍有效）。
- **只加绝对过期、不刷新活跃**：与既有内存实现的空闲续期行为不一致，会让活跃用户被硬性登出。

## 代价与限制

- 每次校验都写一次 `last_seen_at`（行级写）；这是空闲续期的代价，是潜在热点，但单站点规模可接受。
- 会话表随登录累积；靠 TTL、容量上限与过期清理约束，不再依赖「重启即清空」。
- 从备份恢复会同时恢复 `sessions` 行：恢复流程必须显式撤销会话，否则旧 cookie 可能复活（见 [备份与恢复](../operations-and-recovery.md)）。
- 多实例共享会话因此具备条件，但登录限流与 OAuth 尝试仍是进程内存；宣称支持多实例前需一并处理，不在本决策范围。
