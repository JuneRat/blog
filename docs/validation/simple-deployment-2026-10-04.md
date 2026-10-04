# 单文件 Compose 与数据库连接检查验收

日期：2026-10-04。环境：macOS / OrbStack，Linux arm64 容器，独立随机命名的编排、数据库和数据卷。代码中的数据库与权限测试使用一次性 PostgreSQL 容器，不连接已有站点。

## 结果

- [安装与浏览器恢复全链路](simple-deployment-2026-10-04.json)：64.61 秒，全部通过，包含实际 Chromium 安装及备份恢复页面。
- [旧脚本兼容回归](simple-deployment-legacy-2026-10-04.json)：13 个阶段全部通过，包含 STARTTLS 邮件、旧独立恢复、分离数据库账号。
- Rust 安装进程集成测试：13 通过、0 失败；前端安装页面测试：4 通过。
- 全目标 Clippy 禁止警告、前端与 E2E 类型检查、Rust 格式、依赖边界及迁移清单检查通过。
- 发布工具、镜像一致性、旧初始化及备份 worker 的 34 项定向测试通过；工作流/Compose YAML 和内嵌 shell 语法通过。
- Python 全套工具测试共 177 项，3 项因依赖条件跳过，无失败；发布结果页的实际渲染检查通过。

镜像标签为 `942ef03`，包含该提交的安装向导，以及当时工作区中移除 `panel_init.py` 的 Dockerfile 变更；不是纯 `942ef03` checkout 的构建。实际不可变镜像 ID 为 `sha256:df3372afcade584d74898faecf26f474258b1b7af7dc21b7207a6898349eba47`。报告记录两个 Compose 示例及执行脚本的 SHA-256；之后移除测试脚本未使用的 import 不影响行为。

## 覆盖行为

1. 使用 `compose.postgres.yaml` 创建数据库，但不向博客传入 `DATABASE_URL`，也不预写博客配置。
2. 安装检查拒绝缺失安装码、跨源、错误连接、已占用数据库、缺少建表/扩展权限的账号；成功检查不写配置、不创建表。验证成功后撤销权限，正式安装仍会拒绝。
3. 实际浏览器验证安装码、填写数据库地址、验证连接后展开管理员表单，再完成安装并进入后台登录。
4. 安装保存正确连接到 `config.toml`，权限为 `600`；重启读取已保存配置，安装入口不会重新开放。
5. 使用只含博客的 `compose.yaml`，在数据库尚未准备好时安装页即可访问；之后接入独立 PostgreSQL，完成全新环境恢复并通过重启验证。
6. 浏览器备份、加密、下载/上传校验、权限撤销、原地恢复、中断后的维护阻断及回滚、S3 协议往返、远程故障保留本地副本、数据库停机应急入口全部通过。
7. 旧编排内容的容器回归及生成的旧式恢复目录均通过；中断测试同时覆盖 `compose.legacy.yaml` 与 `compose.yaml` 两种文件选择，确认先停止操作容器再重新开放源站。
8. 发布工具仅推送通过身份校验的单个博客镜像，输出镜像身份报告，不复制部署配置、脚本或 `.env`。错误提交、来源、架构、推送或拉回核验失败均不会产生成功报告。

## 复现与边界

```sh
docker build --build-arg VCS_REF="$(git rev-parse HEAD)" -t blog:verify .
python3 -B scripts/test_browser_compose.py --image blog:verify --browser --report browser.json
python3 -B scripts/test_compose.py --image blog:verify --ops-image blog:verify --smtp-security starttls --report compose.json
PYTHONPATH=scripts python3 -B -m unittest scripts/test_registry_release.py scripts/test_container_images.py scripts/test_compose_init.py scripts/test_browser_recovery.py
```

`--browser` 需要已安装前端依赖与 Playwright Chromium。Docker 验收结束后清理自己的容器和卷。实际 GitHub/GHCR 发布、用户 1Panel 服务器、域名/证书及真实 S3 服务尚未执行；发布流程的 Linux amd64 验收由 GitHub Actions 运行。本地小数据集不能代表生产备份窗口与内存需求。
