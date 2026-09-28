# 新库全链路验收

[scripts/acceptance.py](../scripts/acceptance.py) 从真实首次安装开始，通过 HTTP 创建业务数据，再调用正式备份恢复工具完成往返验证。它启动编译后的服务和后台生产资源，使用两个随机命名的独立数据库；不读取现有站点的安装配置、媒体或开发库。

这套验收适合数据库重构、安装流程及跨模块改动后的回归。业务样本通过安装和管理接口写入；SQL 只负责测试库管理、查询持久化结果和恢复工具自身的操作。

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
| 首次安装 | 未安装重定向、未就绪健康检查、安装码提交、空库建表、首个 Owner 权限与审计、私有配置文件、完成后关闭入口、后台 JS/CSS 资源可读 |
| 身份与会话 | 密码登录、资料版本冲突、修改资料保持登录、改密轮换会话及旧 Cookie 失效、仅靠保存配置重启后会话仍有效 |
| 媒体和写作 | 原件上传与公开读取、分类父子关系、标签、同一文章加入两个系列、文章和页面正文 HTML 持久化、封面/头像/logo、两套主题的公开读取、目录/feed/sitemap 排除私密内容 |
| 回收站 | Post/Page 删除后隐藏、恢复为原地址草稿、重新发布；媒体软删除保留公开链接和引用，禁止新引用；恢复前保留软删除媒体样本 |
| 评论 | 游客待审与审核、三层父子/根关系、HTML 持久化、根评论删除后匿名占位、后代在第二展示层查询、公开响应不含邮箱/IP/源文、关闭评论仍保留历史 |
| 备份恢复 | 停止脚本启动的唯一写入进程后备份，校验清单和文件，恢复到另一新库，对照全表计数、媒体原件与引用、安装完成标记，会话清空 |
| 隔离核验 | 普通启动和预约任务被数据库隔离标记拒绝；核验模式允许登录与公开读取，到期内容保持预约状态 |
| 重新开放 | 停止核验进程、解除隔离并再次撤销会话、保存配置切向恢复库后仍不开放安装、预约内容发布、恢复后继续写作和恢复媒体、退出登录 |

HTTP 客户端不自动跟随重定向，避免将错误跳转后的 200 响应误判为成功。数据库和媒体清理仅作用于本次创建的资源；恢复失败时，需要数据库隔离标记与本次私有恢复目录的标记一致，才自动删除目标库。失败和常规中断会执行清理；无法确认归属或清理失败会返回失败，强制杀死进程或断电仍需人工检查临时资源。

## 报告与 CI

报告记录每步结果和耗时、Git 提交与已有跟踪文件改动状态、服务端/后台入口/验收脚本 SHA-256、迁移版本及校验和、备份行数、媒体和引用核验数。失败响应只记录路径、状态码和机器错误码；不输出数据库凭据、安装码、密码、Cookie、CSRF token 或原始服务日志。临时配置、服务日志、媒体和备份在运行结束后清理。

[CI](../.github/workflows/ci.yml) 的 `web` job 构建后台，`acceptance` job 下载相同产物，在 PostgreSQL 18 上执行此流程，并保存 `acceptance-report`。工具自身的测试覆盖配置隔离、资源归属、清理失败及报告敏感信息保护。

这套流程验证实际 HTTP/SSR 和静态资源，不执行浏览器中的 JavaScript 交互，也不替代真实域名、移动端排版、部署停写能力、容量及 RPO/RTO 验收。生产项仍在[路线图 M5](product-roadmap.md#3-里程碑与验收入口)单独确认。脚本通过后不会自动重建或切换开发数据库。

候选版本按[上线验收模板](release-acceptance-template.md)补充浏览器关键路径和目标环境证据；持续读取/保存/系列重排使用[混合容量工具](public-read-capacity.md)。两者的通过范围分别记录，不能由本脚本的 `passed` 推定已完成浏览器与容量验证。
