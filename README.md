# blog

基于 Rust 和 PostgreSQL 的自托管博客，支持 Markdown 写作、主题切换和后台备份恢复。公开站点采用服务端渲染，管理后台使用 React。

[使用文档](docs/README.md) · [部署指南](docs/1panel.md) · [发布版本](https://github.com/JuneRat/blog/releases) · [更新记录](CHANGELOG.md)

## 功能

- **写作**：文章与独立页面、Markdown 编辑、分类 / 标签 / 系列、草稿与预约发布。
- **站点管理**：媒体库、主题管理、原生评论与审核、RSS、sitemap 和 SEO 元数据。
- **多人协作**：本地密码与 OAuth 登录、角色权限，以及通过 SMTP 发送邀请和密码找回邮件。
- **备份恢复**：在后台手动或定时备份，支持 S3 兼容存储、上传备份和原地恢复。

## 快速开始

推荐使用 1Panel 或 Docker Compose 部署。官方镜像支持 **Linux amd64 / x86_64**，可免登录拉取。

从[最新 Release](https://github.com/JuneRat/blog/releases/latest) 下载一份完整的 Compose 文件，镜像地址已固定到对应发布版本：

| 数据库方案 | 下载文件 |
| --- | --- |
| 连接已有 PostgreSQL | [compose.yaml](https://github.com/JuneRat/blog/releases/latest/download/compose.yaml) |
| 同时启动 PostgreSQL | [compose.postgres.yaml](https://github.com/JuneRat/blog/releases/latest/download/compose.postgres.yaml) |

在 1Panel 中：

1. 进入「容器 → 编排」，粘贴所选文件，填写域名、安装码及所需的数据库密码，启动服务。
2. 为域名配置 HTTPS 反向代理，访问 `https://你的域名/install`。
3. 在安装向导中验证 PostgreSQL 空库连接、创建管理员并完成安装。

配置自动保存到持久卷，安装完成后访问 `/admin/` 登录后台。详细步骤见 [1Panel 部署](docs/1panel.md)，其他部署方式见 [Docker Compose 指南](docs/docker-compose.md)。

## 文档与开发

- [后台备份与恢复](docs/browser-backup.md)
- [主题与渲染](docs/themes-and-rendering.md)
- [本地开发](docs/development.md) · [后台开发](docs/admin-development.md)
- [配置参考](docs/configuration.md) · [管理 API](docs/admin-api.md) · [架构说明](docs/architecture.md)

问题反馈与功能建议请提交 [GitHub Issue](https://github.com/JuneRat/blog/issues)。
