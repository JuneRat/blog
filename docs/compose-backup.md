# Compose 备份与恢复

入口为 `sh scripts/compose-backup.sh`。所有配置沿用部署目录的 `.env`，脚本自动设置私有文件权限。常驻应用不安装 Python/数据库管理工具；按需运行的 ops 镜像包含同版本博客、PostgreSQL 18 客户端、恢复脚本和 restic，不挂载 Docker socket。

## 先准备匹配的镜像

源码部署在原部署目录执行一次：

```sh
docker compose build blog ops
```

交付包部署通过 `docker load` 加载镜像，并将包内 `IMAGE`、`OPS_IMAGE` 的值填入 `.env` 的 `BLOG_IMAGE`、`BLOG_OPS_IMAGE`。升级时二者一起更换。恢复校验二进制 SHA-256 和迁移校验和；旧备份先用匹配版本恢复，再执行升级，不能通过忽略校验跨版本导入。

## 备份和状态

```sh
sh scripts/compose-backup.sh backup
sh scripts/compose-backup.sh status
sh scripts/compose-backup.sh verify backups/blog-具体时间与编号.tar.gz
```

备份入口会检查镜像和配置，停止本项目的 blog，确认源数据库没有其他客户端连接，导出数据库并复制媒体、已安装主题和配置。结构、媒体登记/引用及文件校验全部通过后，才原子发布 `backups/blog-*.tar.gz`。随后先启动原服务，再执行可选的异地同步及保留清理；普通失败和 TERM/INT 也会尝试重新启动原服务。

只支持同一 Compose 项目的 `db:5432`、默认 `serve` 命令和标准配置/媒体挂载路径；命令行覆盖或改变这些容器路径会在预检中拒绝。备份前须停用外部 SQL 写入者和另行调度的维护任务；连接检查不能阻止外部程序在检查后重新连接。本脚本的备份、恢复、保留期维护与媒体清理通过同一目录锁互斥，其他运维入口也应避开备份维护窗口。

备份包包含数据库、媒体、主题、源 TOML、原 `.env`、应用容器的实际环境和镜像/版本信息；环境覆盖及自定义 OAuth 密钥也能保存。**本地包包含明文秘密**，目录自动设为 700、文件为 600，只应放在受控磁盘。秘密在同一个受控恢复包中形成完整恢复材料；通用宿主机恢复流程仍可选择分开保存。异地存储通过下文 restic 加密。

`backups/status.json` 记录最近操作、阶段、开始/结束时间、成功/失败和备份文件名；`last-successful-backup.json` 保留最近一次完整备份成功记录。默认本地保留最近 7 份，可在 `.env` 设置 `BLOG_BACKUP_KEEP`。未完成的包不会参与保留清理；异地上传失败时命令非零退出，本地包保留、旧备份不清理。

断电或 SIGKILL 无法执行 shell 清理，可能留下停止的博客和 `backups/.operation-lock`。先检查 `docker compose ps --all`、相关 ops 进程、锁内 owner 和状态文件；确认没有操作仍在运行后，启动原服务并移除该锁，再重试。失败的 `.partial` / 点号开头的临时目录只在确认无运行任务后处理，不应删除数据卷。

## 恢复到独立部署

新目录必须不存在，父目录须可写：

```sh
sh scripts/compose-backup.sh restore backups/blog-具体时间与编号.tar.gz /srv/blog-restored
cd /srv/blog-restored
sh scripts/compose-backup.sh check 你的管理员用户名
sh scripts/compose-backup.sh release
```

`restore` 创建新的部署目录、单份 `.env`、随机 Compose 项目名和独立数据卷。数据库管理员、结构所有者、普通运行账号及维护账号都使用新生成的密码；应用以受限 `blog_app` 连接。原部署不切库、不删卷、不改密码。新部署保留原站点地址与 OAuth 密钥，使用备份对应的不可变本地应用镜像 ID。原部署的完整秘密材料保留在私有备份包内，不复制到恢复后 HTTP 服务可读的配置卷。

恢复只启动数据库，在新建的随机 `blog_restore_*` 库中导入，撤销备份中的会话，核对数据数量、Owner、结构及媒体引用，并写入数据库隔离标记。不要提前运行 `docker compose up` 启动应用；隔离标记会阻止普通服务开放。

`check` 隐藏输入管理员密码，在 ops 容器的 loopback 地址启动非 root 博客，不发布宿主机端口，也不运行预约发布。检查 readiness、登录、后台首页、站点首页、最多 10 个已发布页面，以及全部未删除媒体的 HTTP 内容校验和；数据库/磁盘层的媒体和引用检查覆盖全部记录。检查结束后停止临时进程并清空验证会话。自动化可使用 `--password-stdin`，不要把密码放入命令行参数。

`release` 只接受已经通过 check 的恢复，重查实际媒体卷和数据库、再次清空会话并解除隔离，然后启动新项目的 blog。默认随机分配宿主机 loopback 端口，避免与原站点冲突，命令会输出地址；它不会修改反向代理或公网域名。核对预约内容和访问情况后，在新 `.env` 固定 `BLOG_HTTP_PORT`，重建容器并切换代理。新部署本身即可继续执行备份；需要异地备份时，在它的 `.env` 重新配置下节参数。

还原失败会留下新项目供检查，原站点仍独立运行；不要用删除原卷的方式重试。验证失败时保持隔离，可修复问题后重新 check；重新做完整还原则选择另一个不存在的目录。若 release 已解除隔离但启动失败，修复启动问题后在该恢复目录执行 `docker compose up -d --no-build --wait blog`，无需重复解除隔离。

## 加密异地副本

在现有 `.env` 设置私有 S3 兼容仓库与凭据：

```dotenv
RESTIC_REPOSITORY=s3:https://s3.example.com/private-backups/blog
RESTIC_PASSWORD=在密码管理器生成并另外保存的长随机密码
AWS_ACCESS_KEY_ID=你的访问凭据
AWS_SECRET_ACCESS_KEY=你的秘密凭据
AWS_DEFAULT_REGION=us-east-1
BLOG_BACKUP_REMOTE_KEEP=30
```

仓库首次使用执行一次：

```sh
sh scripts/compose-backup.sh remote-init
sh scripts/compose-backup.sh backup
sh scripts/compose-backup.sh remote-list
```

已配置仓库时，每次成功备份自动加密上传。远端默认保留最近 30 份，按站点安装 ID 分组清理，不清理其他站点。上传失败可用 `sync blog-具体时间与编号.tar.gz` 重试。仓库密码和访问凭据须另存到服务器之外，避免服务器丢失后无法获取备份。[restic 仓库与 S3 配置](https://restic.readthedocs.io/en/stable/030_preparing_a_new_repo.html#s3-compatible-storage)

整台服务器丢失后：在新机器加载匹配的交付镜像和部署文件，初始化 `.env`，填回异地仓库凭据，然后获取明确的快照 ID：

```sh
sh scripts/compose-backup.sh remote-list
sh scripts/compose-backup.sh fetch 快照ID
sh scripts/compose-backup.sh restore backups/取回的文件名.tar.gz /srv/blog-restored
```

fetch 会解密、检查文件清单和匹配版本后才发布本地包，拒绝覆盖已有包。备份不嵌入应用镜像，必须独立保留相应交付包。加密仓库也可以是挂载磁盘，但同机副本不能覆盖整机丢失的场景。

## 定时运行

Linux 可使用 [blog-backup.service](../ops/blog-backup.service) 和 [blog-backup.timer](../ops/blog-backup.timer)。先将 service 的工作目录、脚本路径和 `User` 改为实际部署目录及其所有者，该用户须可访问 Docker，再安装到 systemd 并启用 timer；默认按服务器时区每天 03:30 执行，随机延迟最多 5 分钟，支持漏跑补执行。

```sh
sudo install -m 644 ops/blog-backup.service ops/blog-backup.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now blog-backup.timer
systemctl list-timers blog-backup.timer
journalctl -u blog-backup.service
```

保留期维护使用 `sh scripts/compose-backup.sh maintenance`，在现有 `.env` 配置独立维护连接；配套 `ops/blog-maintenance.service` / `.timer` 同样通过 Compose 运行，并和备份互斥。正式媒体清理的 `media-plan` / `media-apply` 入口见[运维说明](operations-and-recovery.md#正式媒体物理清理)。

也可用现有调度器运行同一个 backup 命令。调度器应报告非零退出，并监控最近成功备份是否过期；脚本不会自动创建通知渠道。仓库测试覆盖加密仓库存取、损坏/缺文件拒绝、失败后原服务重启、独立项目恢复、登录/页面/图片与会话撤销。实际备份耗时、恢复耗时、磁盘空间、远端权限和业务恢复点仍由生产演练记录。

恢复保留 TOML 中的连接池/超时策略及显式注入的 `BLOG_DB_*` 环境变量，只替换数据库地址与迁移目录；新环境资源规格不同时可在核验前调整池大小。
