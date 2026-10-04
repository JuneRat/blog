# 网页备份、原地恢复与 1Panel 模板验收

日期：2026-10-04。环境：macOS / OrbStack，Linux arm64 容器，Compose 5.1.2。使用随机命名的隔离编排、独立数据库和数据卷，结束后只清理测试资源。

## 结果与构建身份

- [网页全链路报告](browser-recovery-2026-10-04.json)：全部通过，61.834 秒，包含真实 Chromium 页面验证。
- [现有 Compose 回归报告](browser-recovery-legacy-compose-2026-10-04.json)：13 个阶段全部通过，287.301 秒，SMTP 使用容器内 STARTTLS 测试服务。
- 网页验收镜像 ID 为 `sha256:f22566f3c8eb0afacee8323478641a758baa99891958b023c83d46a237138de5`。这是本地工作区构建：镜像 revision 标签为 `e3d13b3`，实际还包含随后提交为 `c5a4e4f` 的恢复权限修复。该标签不代表未经修改的 `e3d13b3` 提交；只修改注释的后续差异不影响执行行为。报告记录了实际镜像 ID 及脚本校验和。
- 现有 Compose 回归使用较早的同一实现工作区镜像（标签 `51c35e3`），实际 ID 和阶段保留在报告中。之后针对上传授权、上传清理和恢复权限的改动由最后一次网页验收覆盖。

本记录为本地隔离验收，不是已发布镜像或生产部署证明。发布工作流会在 Linux amd64 重新构建候选提交，完成 STARTTLS、TLS 和网页 HTTP 验收，再发布同一个已验收镜像；不会在发布时重新构建。

## 网页验收覆盖

1. 直接使用 `ops/1panel/compose.yaml` 启动，初始化服务生成数据库凭据；通过安装码在 HTTP 创建管理员。
2. 拒绝访客、普通账号、跨源请求及错误 CSRF；改密后旧恢复会话的状态读取返回 401。
3. Chromium 操作恢复密钥下载、重新选择确认、创建及验证备份；检查移动视口没有横向溢出。
4. 生成 age 加密备份，下载、分块上传与完整校验；损坏备份校验失败，原站仍可用。
5. 修改文章后原地恢复，确认内容回到备份状态、生成加密恢复前副本、旧业务会话失效。
6. 在数据库事务完成但文件替换之前人为中断进程；重启后业务仍为 503，拒绝跳过恢复，使用恢复前副本成功回滚。
7. 本地 S3 协议服务的连接测试、上传、列表、下载与重新校验；远程故障保留本地副本、报告警告且站点已经开放。
8. 停止数据库并重启博客，恢复页面仍可打开，密钥仍可授权；数据库重启后通过网页重新启动站点，策略保持。
9. 新建另一组空卷，仅用安装码、上传备份和恢复密钥完成恢复，重启后内容及应急密钥仍可用。
10. 公共任务状态和应用日志不含测试密码、私钥或远程访问凭据。

恢复中断和 S3 模拟脚本仅通过测试只读挂载提供，不打入生产镜像。

## 其他检查

| 检查 | 结果 |
|---|---|
| Rust 全工作区测试（独立 PostgreSQL） | 821 通过，3 忽略，0 失败，共 78 组 |
| Rust Clippy 全目标、禁止警告 | 通过，包含恢复权限修复 |
| 后台单元测试 | 45 个文件、440 个测试通过 |
| 后台类型检查及 HTTP 契约生成检查 | 通过 |
| Python 工具测试 | 178 个测试，3 跳过，无失败；涵盖 18 项恢复 worker 测试 |
| 发布打包、镜像一致性、初始化及恢复 worker 复核 | 通过 |
| Cargo 依赖边界、迁移清单与生成 SQL、格式、补丁空白 | 通过 |
| 工作流 YAML、内嵌 shell 语法、1Panel 编排解析 | 通过 |

## 复现

```bash
docker build --build-arg VCS_REF="$(git rev-parse HEAD)" -t blog:verify .
python3 -B scripts/test_browser_compose.py --image blog:verify --report browser.json
# 已安装前端依赖与 Playwright Chromium 时，额外执行实际浏览器检查：
python3 -B scripts/test_browser_compose.py --image blog:verify --browser --report browser-ui.json
python3 -B scripts/test_compose.py --image blog:verify --ops-image blog:verify --smtp-security starttls --report compose.json
```

真实 1Panel 界面、域名/证书、互联网 SMTP 与实际 S3 供应商尚未在用户服务器验证。模板本身已通过实际 Docker Compose 部署。当前格式只接受已知且一致的迁移校验和集合；测试小数据集不代表大站点的备份窗口、磁盘或内存需求。
