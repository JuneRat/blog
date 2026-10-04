# 在 1Panel 中部署

复制一份 Compose，填写域名、安装码并启动博客，再通过安装向导连接数据库。示例已固定经过验收的 `ghcr.io/junerat/blog` 镜像，适用于 x86_64 / amd64 服务器。无需下载配置包、克隆源码或在服务器执行初始化脚本。

## 选择编排

| 数据库在哪里 | 使用的完整示例 |
|---|---|
| 已在 1Panel 创建，或使用其他 PostgreSQL 实例 | [compose.yaml](../compose.yaml)，只运行博客 |
| 希望随博客一起运行 PostgreSQL | [compose.postgres.yaml](../compose.postgres.yaml)，运行博客和数据库 |

两个文件是独立的完整示例，选择其中一个即可。数据库连接始终在安装向导填写；Compose 不向博客传入数据库密码。带数据库的示例在新数据库卷中创建非超级用户 `blog_owner`，安装时使用它。

## 填写并启动

在 1Panel“容器 → 编排”中创建编排，粘贴所选文件，修改这些值：[1Panel 编排说明](https://1panel.pro/docs/v2/user_manual/containers/composes/)

- `BLOG_PUBLIC_BASE_URL`：填写实际访问地址，例如 `https://blog.example.com`。
- `BLOG_INSTALL_TOKEN`：填写自己保存的私有安装码，20–256 个 ASCII 字符。
- `ports`：默认宿主机端口为 `8080`；多站点分别选择未占用的端口。
- 使用带数据库示例时，还需填写 `POSTGRES_PASSWORD`（数据库管理密码）与 `BLOG_OWNER_PASSWORD`（博客数据库账号密码），二者不要相同。

`blog.image` 已填写完整镜像地址，首次部署保持原值即可。本项目镜像已公开，Docker 会在本机缺少对应版本时自动拉取，无需登录镜像仓库。

自行发布到其他 GHCR 仓库时，公开源码不会自动公开镜像；包所有者需单独设为 Public，或在 1Panel 配置私有仓库凭据。如果拉取提示 `unauthorized` 或 `denied`，先检查镜像包的可见性与地址。[GHCR 访问说明](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry)

当前发布流程支持 Linux amd64，ARM 服务器需使用相应架构的本地构建。镜像发布方法见 [GHCR 发布](docker-compose.md#从-ghcr-拉取镜像并部署到-1panel)。

每个站点使用独立编排名和数据卷。首次安装前健康检查显示未就绪是正常的，安装页面仍然可访问。

## 配置域名并安装

将域名解析到服务器，在 1Panel 的“网站”中创建反向代理网站，绑定域名并启用 HTTPS。[1Panel 创建网站说明](https://1panel.pro/docs/v2/user_manual/websites/website_create/)

代理使用宿主机网络时，目标为 `http://127.0.0.1:8080`，端口与编排一致。代理使用独立容器网络时，需连通相应内部网络。请求体上限至少设为 4 MiB，网页恢复会分块上传较大的备份。

打开 `https://你的域名/install`：

1. 输入编排中的安装码。
2. 填写 PostgreSQL 连接地址，点击“验证数据库连接”。会验证网络、账号、空库及建表/扩展权限，不会创建表或保存配置。
3. 验证通过后填写管理员账号与密码，点击“安装博客”。服务重新检查数据库后保存配置并执行安装。

带数据库示例使用 `postgres://blog_owner:密码@db:5432/blog`，密码取 `BLOG_OWNER_PASSWORD`。已有数据库使用博客容器能够访问的主机名或 IP；容器中的 `localhost` 指向博客容器自身。同一 Docker 网络可以通过数据库容器名连接，1Panel 中单独创建的数据库需先配置可达网络。密码含 URL 特殊字符时使用百分号编码。

数据库连接与站点地址保存在配置卷的 `/var/lib/blog/config/config.toml`，权限为 `600`；重启或更新博客镜像时继续使用。向导不会创建数据库本身，只初始化用户已准备好的空库。安装完成后入口关闭，进入 `/admin/` 登录。

已有备份时，可从安装页选择“从备份恢复站点”，使用安装码进入应急页面，并提供目标数据库连接、备份与恢复密钥。

## 更新与日常操作

备份、定时计划、远程存储和原地恢复均在后台“系统 → 备份与恢复”操作，见[使用说明](browser-backup.md)。

更新前创建并下载备份，然后修改原编排中的博客 `image`，通过 1Panel 拉取并重新创建博客服务。保留编排名、数据库及配置/媒体/主题数据卷；更新博客无需重建数据库。已有旧版编排继续沿用它的数据库和数据卷，只更新博客镜像，不用新模板替换原有卷定义。

本站结构所有者连接会在启动时执行支持的数据库迁移。回退镜像本身不会回退数据库，恢复时需选用与备份结构兼容的版本。
