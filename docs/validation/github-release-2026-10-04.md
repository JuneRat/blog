# GitHub 与 GHCR 首次交付验收（2026-10-04）

公开源码仓库：[JuneRat/blog](https://github.com/JuneRat/blog)。本次镜像来自提交 `dd38b0e3f2f3e294b2e3c872b212092ec45ebc5f`，目标架构为 `linux/amd64`。

```text
ghcr.io/junerat/blog:sha-dd38b0e3f2f3e294b2e3c872b212092ec45ebc5f
ghcr.io/junerat/blog@sha256:08641a14123c345fee6e66d08003fbd7b4dc543ef9bc0a11a731549f6b4a264e
```

云端验收记录的镜像 ID（config digest）为 `sha256:290c9d7f2e77a9229cd15184491513de2557c0262f82c48107bf083c6016e023`。[发布工作流](https://github.com/JuneRat/blog/actions/runs/37189679951)在同一个镜像通过全部容器验收后，将它交给发布任务，核对校验和、镜像 ID、提交、来源及架构，推送 GHCR，并使用 digest 拉取确认镜像 ID 一致。没有重新构建发布镜像。

## 验收结果

- [完整 CI](https://github.com/JuneRat/blog/actions/runs/37189652241)：`check`、`web`、`security`、`acceptance` 全部通过，包含 Rust、前端、脚本、数据库 TLS、实际浏览器流程、SMTP 与独立恢复演练。
- STARTTLS 容器验收：13 个阶段全部通过，约 266 秒，覆盖安装、发布、主题、任务、迁移、加密备份、失败后重启、独立恢复和恢复后的邮件发送。
- 隐式 TLS 容器验收：相同 13 个阶段全部通过，约 266 秒。
- 网页恢复接口验收：约 44 秒，覆盖数据库连接验证及配置保存、原地恢复、中断回滚、应急访问、新环境恢复、外置数据库、S3 协议往返和凭据日志检查。

容器验收报告保存在发布工作流的 `compose-verification-dd38b0e3f2f3e294b2e3c872b212092ec45ebc5f` artifact；用户部署不需要下载该 artifact。

首次 Linux 验收中发现的问题已分别提交修复：数据库测试使用可精确持久化的时间；前端与渲染容量测试使用适合 CI 的等待时间；旧维护脚本兼容 Compose 2 的容器重启方式；独立恢复复制的公开数据库初始化脚本可供 PostgreSQL 账号读取，配置和凭据保持私有。上述修复均包含针对性回归验证。

## 部署文件与验证边界

[compose.yaml](../../compose.yaml)用于已有数据库，[compose.postgres.yaml](../../compose.postgres.yaml)包含 PostgreSQL。两份模板已将占位镜像替换为本次发布的 digest；数据库和安装配置继续由部署者填写。最终模板使用 Compose 2.38.2 解析验证，部署步骤见 [1Panel 指南](../1panel.md)。

发布工作流先验证登录 GHCR 后按 digest 拉取。本地随后使用临时空 Docker 认证配置，实际匿名拉取完整的 `linux/amd64` 镜像，并核对仓库 digest、架构和提交一致，确认目前可免登录拉取。匿名取得的 manifest 内容 SHA-256 与固定 digest 一致，其中 `config.digest` 与云端验收记录的镜像 ID 一致。此检查没有更改包的可见性。

本地 Docker 29.4.0 的 `image inspect .Id` 返回 manifest digest，云端 Docker 28 使用 config digest，因此不能直接跨这两个环境比较 `.Id` 字段；应核对上述 manifest 与 config 的对应关系。[Docker containerd 镜像检查实现](https://github.com/moby/moby/blob/master/daemon/containerd/image_inspect.go)

真实 1Panel 服务器、域名证书、互联网 SMTP 和实际 S3 供应商不在本次隔离验收范围内。
