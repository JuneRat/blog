# 新库全链路验收

[scripts/acceptance.py](../scripts/acceptance.py) 从真实首次安装开始，通过 HTTP 创建业务数据，再调用正式备份恢复工具完成往返验证。它启动编译后的服务和后台生产资源，使用两个随机命名的独立数据库；不读取现有站点的安装配置、媒体或开发库。

这套验收适合数据库重构、安装流程及跨模块改动后的回归。业务样本和任务请求通过安装和管理接口写入；SQL 负责测试库管理、查询持久化结果、恢复操作，以及随机验收库中明确列出的旧 HTML、过期记录和到期时间夹具。原生验收同时启动有容量与大小限制的回环 SMTP 收件器，邮件仅保留在进程内存中，不转发到外部地址、不写入报告；不继承现有 SMTP 配置，恢复核验关闭邮件功能。

## 运行

需要 Rust 服务端、后台生产构建和独立 PostgreSQL 18 测试实例。`BLOG_TEST_ADMIN_URL` 必须显式指定 loopback 管理连接，账号需能创建和删除测试数据库；不接受 URL 查询参数覆盖连接地址。

可以沿用已有的本地测试实例，也可以启动临时容器：

```bash
docker run --rm -d --name blog-acceptance-db \
  -e POSTGRES_USER=blog -e POSTGRES_PASSWORD=blog -e POSTGRES_DB=postgres \
  -p 127.0.0.1:55433:5432 postgres:18-alpine
```

等待 `docker exec blog-acceptance-db pg_isready -U blog -d postgres` 成功后，在仓库根目录运行：

```bash
cargo build -p server --bin blog
pnpm --dir apps/admin build

BLOG_TEST_ADMIN_URL=postgres://blog:blog@127.0.0.1:55433/postgres \
BLOG_TEST_PG_CONTAINER=blog-acceptance-db \
python3 -B scripts/acceptance.py --report /tmp/blog-acceptance.json
```

`BLOG_TEST_PG_CONTAINER` 让备份工具在指定容器内执行，必须与管理 URL 指向同一实例。使用本机 PostgreSQL 工具时省略它，并确保 `psql`、`createdb`、`pg_dump` 和 `pg_restore` 的版本适配 PostgreSQL 18。

可用 `--binary` 和 `--admin-dist` 指向其他构建产物。`--report` 可省略；指定时必须是新文件，重跑需要换路径。脚本退出码为 0 才表示全部步骤和资源清理成功。

追加 `--browser` 可在同一个临时站点上执行生产构建的 Playwright 冒烟测试。首次运行先执行 `pnpm --dir apps/admin exec playwright install chromium`（Linux CI 使用 `--with-deps`）；本机已有 Edge 时可以设置 `PLAYWRIGHT_CHANNEL=msedge`。浏览器只使用本次生成的测试账号，关闭重试，失败截图和 trace 写入 `apps/admin/test-results/`。

若是专为本次验收创建的上述容器，验收结束后停止它：

```bash
docker stop blog-acceptance-db
```

完整提交前检查也可追加这套流程：

```bash
BLOG_ACCEPTANCE_TEST=1 \
BLOG_TEST_ADMIN_URL=postgres://blog:blog@127.0.0.1:55433/postgres \
BLOG_TEST_PG_CONTAINER=blog-acceptance-db \
./scripts/check.sh
```

`BLOG_ACCEPTANCE_TEST` 会构建服务端和后台后执行 HTTP 验收；`BLOG_RECOVERY_TEST` 控制已有的数据库角色、清理失败与恢复异常演练，两者可同时开启。

## 验收范围

| 阶段 | 自动验证 |
|---|---|
| 首次安装 | 未安装重定向、未就绪健康检查、安装码提交、空库建表、首个 Admin 权限与审计、私有配置文件、完成后关闭入口、后台 JS/CSS 资源可读 |
| 身份与会话 | 密码登录、资料版本冲突、修改资料保持登录、改密轮换会话及旧 Cookie 失效、仅靠保存配置重启后会话仍有效 |
| 邮件（原生） | 通过实际 SMTP 协议捕获邀请与找回邮件，验证配置地址生成链接、首次设置密码、一次性链接重放拒绝、找回响应不枚举账号、旧会话失效、撤销作者角色后写入被拒 |
| 浏览器（`--browser`） | 生产 SPA 密码登录、正文预览、发布与公开阅读、编辑深链刷新、取消离开、刷新后恢复本机草稿、保存期间继续输入；任务页面未来 HTML 计划、刷新后同 ID、取消、实际失败定位、修复后的新 ID 重试与历史详情 |
| 媒体和写作 | 原件上传与公开读取、分类父子关系、标签、同一文章加入两个系列、文章和页面正文 HTML 持久化、封面/头像/logo、Default 与通过 ZIP 验证、安装、激活的第三方 Paper 主题公开读取、目录/feed/sitemap 排除私密内容 |
| 修改草稿与历史（原生） | Post/Page 已发布编辑与公开正文隔离、过期版本拒绝、显式发布、历史恢复为新的修改草稿、恢复后再发布；备份包含尚未发布的修改草稿和历史，隔离核验及重新开放后均检查线上与编辑副本 |
| 主题升级（原生） | 临时配置主题的真实首页预览不改变激活状态；升级保留身份、配置和选择版本，上一版可回退，旧版本回退请求拒绝；主题及上一版目录随备份恢复 |
| 回收站 | Post/Page 删除后隐藏、恢复为原地址草稿、重新发布；媒体软删除保留公开链接和引用，禁止新引用；恢复前保留软删除媒体样本 |
| 评论 | 显式开启游客评论后验证待审与审核、三层父子/根关系、HTML 持久化、根评论删除后匿名占位、后代在第二展示层查询、公开响应不含邮箱/IP/源文、关闭评论仍保留历史 |
| 持久任务 | 同一 queued HTML 请求跨进程重启到期后执行，业务版本/更新时间不变且审计一次；实际失败和新 ID 重试保留旧报告；启用周期清理后确实清空过期 IP、删除过期审计并保留新数据；固定周期实际发布到期 Post/Page |
| 备份恢复 | 停止脚本启动的唯一写入进程后备份，校验清单和文件，恢复到另一新库，对照全表计数、媒体原件、当前及历史引用、安装完成标记，会话和一次性邮件链接清空 |
| 隔离核验 | 普通启动和预约任务被数据库隔离标记拒绝；核验模式允许登录与公开读取，到期内容保持预约状态 |
| 重新开放 | 停止核验进程、解除隔离并再次撤销会话、保存配置切向恢复库后仍不开放安装、预约内容发布、恢复后继续写作和恢复媒体、退出登录 |

HTTP 客户端不自动跟随重定向，避免将错误跳转后的 200 响应误判为成功。数据库和媒体清理仅作用于本次创建的资源；恢复失败时，需要数据库隔离标记与本次私有恢复目录的标记一致，才自动删除目标库。失败和常规中断会执行清理；无法确认归属或清理失败会返回失败，强制杀死进程或断电仍需人工检查临时资源。

## 报告与 CI

报告记录每步结果和耗时、Git 提交与已有跟踪文件改动状态、服务端/后台入口/验收与 SMTP 收件脚本 SHA-256、迁移版本及校验和、备份行数、媒体和引用核验数。失败响应只记录路径、状态码和机器错误码；不输出数据库凭据、安装码、密码、Cookie、CSRF token 或原始服务日志。临时配置、服务日志、媒体和备份在运行结束后清理。

[CI](../.github/workflows/ci.yml) 的 `web` job 构建后台，`acceptance` job 下载相同产物，在 PostgreSQL 18 上执行含 `--browser` 的流程，并保存 `acceptance-report`，失败时另保存浏览器证据。工具自身的测试覆盖配置隔离、资源归属、清理失败及报告敏感信息保护。

任务场景由原生与 [Compose 验收](../scripts/test_compose.py)共用。重启检查先停止本次唯一写入者，确认未来请求仍为 queued，再仅将该请求的测试到期时间提前，启动同一库并核验原 ID 的业务结果；清理与预约周期同样只提前测试计划的下一次检查时间。这样覆盖真实领取和执行，无需让 CI 等待最短一小时清理周期。HTML 失败由随机库中的旧派生数据引用不存在媒体产生，重试继续调用真实任务接口；浏览器通过真实内容 API 修复源文。所有夹具在本次数据库清理时移除。

默认流程验证实际 HTTP/SSR 和静态资源，`--browser` 另执行上述 JavaScript 关键交互；它们不替代真实 SMTP 服务的 TLS/认证与外部邮箱送达、真实域名、移动端排版、部署停写能力、容量及 RPO/RTO 验收。生产项仍在[路线图 M5](product-roadmap.md#3-里程碑与验收入口)单独确认。脚本通过后不会自动重建或切换开发数据库。

候选版本按[上线验收模板](release-acceptance-template.md)补充浏览器关键路径和目标环境证据；持续读取/保存/系列重排使用[混合容量工具](public-read-capacity.md)。`--browser` 覆盖上表的写作与任务浏览器流程；未启用时不包含浏览器验证，两种运行方式都不替代目标环境验收与容量验证。

原生与 Compose 验收共用 `scripts/acceptance_support.py` 的 HTTP 客户端、内容生命周期、分类、评论和媒体场景；两个入口分别保留本机进程与容器部署的启动、停止和恢复验证。

2026-10-03 的[高、中优先级功能本地隔离验收](validation/priority-features-local-2026-10-03.md)记录固定服务端构建的 12 项真实浏览器流程、本地 SMTP 邀请与找回、修改草稿与历史、主题升级回滚和完整备份恢复，附构建指纹、原始 JSON 及执行日志。

2026-09-30 的[逐项修复本地验收记录](validation/review-fixes-local-2026-09-30.json)关联独立提交、完整 Rust/前端/脚本检查、真实浏览器写作、干净提交的 Docker Compose 安装与加密恢复，以及短时混合并发烟测。查询计划和依赖离线审计另有完整证据；保留已有用户日志改动的本机测试与排除这些改动的干净 Docker 快照分别记录，不能混用构建身份。
