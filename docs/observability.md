# 探针、日志与指标

## 服务与依赖状态

| HTTP 入口 | 安装期间 | 正常运行 | 数据库不可用 |
|---|---|---|---|
| `/livez` | 200 | 200 | 200，只检查 HTTP 进程能响应 |
| `/readyz` | 503 | `SELECT 1` 成功则 200 | 503，整个依赖检查最多等待 2 秒 |
| `/healthz` | 503 | 同 `/readyz` | 同 `/readyz`，兼容已有调用方 |
| `/version` | 200 | 200 | 200，不查询数据库 |

Compose 镜像健康检查使用 `/readyz`；安装完成前显示 unhealthy 是预期行为。数据库故障不应触发按 liveness 重启应用的策略。启动时数据库无法连接会直接退出，以上故障语义指已经启动的服务。

`/version` 返回 `version` 和 `revision`，只包含构建版本与提交，不包含配置、连接串或秘密。Docker 构建参数 `VCS_REF` 编译进二进制；CI 传入完整提交 SHA。源码构建可使用 `BLOG_BUILD_REVISION=<Git SHA> cargo build --release -p server --bin blog`；未提供时明确返回 `unknown`，不会把不确定的提交冒充构建身份。Compose 源码构建通过 `.env` 的 `BLOG_SOURCE_REVISION` 传递这一参数。修改运行期环境不能改变已编译的版本信息。

`livez`、`readyz`、`version`、`metrics` 与原有 `healthz` 一样属于保留页面 slug。

## 结构化日志

原生启动默认文本，设置 `BLOG_LOG_FORMAT=json` 或 TOML `[logging] format="json"` 后使用逐行 JSON；Compose 默认 JSON，可在已有 `.env` 中覆盖为 `text`。`RUST_LOG` / `logging.filter` 继续控制过滤级别。

请求完成日志的 `fields` 含 `request_id`、`method`、`path`、`route`、`status`、`elapsed_ms`，认证成功后另有 `actor_id`；info 请求 `span` 启用时也保留关联上下文。响应头 `x-request-id`、管理错误体和日志中的编号保持一致，即使 `RUST_LOG=warn` 过滤了 info span，5xx 完成记录和管理内部错误仍携带编号。请求完成日志不包含查询参数、Cookie、授权头或请求正文；路径和已验证用户编号仍属于受控日志数据。5xx 记 warn，其余完成记录记 info；请求指标始终全量统计，不受日志过滤影响。

JSON 运行日志进入 stderr，CLI 的机器可读结果仍写 stdout。安装码和启动地址是必须可见的运维提示，即使 `RUST_LOG=warn` 也会显示；JSON 模式下它们同样是 stderr JSON。配置或日志初始化本身失败时，尚未建立日志器，错误仍是 stderr 文本。

Docker `local` driver 管理轮转，每个容器最多配置 5 个 10 MB 日志文件。安装码也在日志内，访问权限与现有部署日志保持一致。原生启动的日志保存与轮转由进程管理器负责。

## 独立指标端口

Prometheus 文本格式只在独立管理监听器的 `/metrics` 提供，业务端口不提供指标。原生运行默认不开监听器，使用 `BLOG_METRICS_BIND=127.0.0.1:9090` 或 TOML `[metrics] bind="127.0.0.1:9090"` 启用。

Compose 内部监听 `0.0.0.0:9090`，宿主机端口固定绑定 `127.0.0.1`，端口号可在已有 `.env` 设置 `BLOG_METRICS_PORT`，默认 9090。此端口仅使用网络隔离，不做登录鉴权：宿主机及同一受信任 Compose 网络可访问；不要把它加入公网反向代理。独立监听失败会使服务启动失败，避免部署显示正常却缺失指标。

```sh
curl -fsS http://127.0.0.1:8080/livez
curl -fsS http://127.0.0.1:8080/readyz
curl -fsS http://127.0.0.1:8080/version
curl -fsS http://127.0.0.1:9090/metrics
```

| 指标 | 含义 |
|---|---|
| `blog_http_requests_total{method,route,status}` | 已返回响应头的请求计数，含 4xx/5xx |
| `blog_http_request_duration_seconds{method,route}` | 请求到响应头的耗时直方图，不含响应体传输时间 |
| `blog_http_requests_in_flight` | 尚未返回响应头的请求数 |
| `blog_http_requests_cancelled_total{method,route}` | 返回响应前被取消的处理，不冒充 HTTP 500 |
| `blog_database_pool_connections{state}` | 运行池 `size`、`idle`、`in_use`、`max` 的采样快照；不包含临时安装连接 |
| `blog_installation_complete` | 运行池已装配为 1，尚未安装为 0；它不是数据库健康探针 |
| `blog_build_info{version,revision}` | 当前构建身份，值为 1 |

标签使用路由模板，如 `/api/admin/posts/{id}`，未知路径统一为 `unmatched`，扩展 HTTP 方法统一为 `OTHER`。不将 slug、用户 ID、请求编号或查询参数放入指标标签。每次进程重启计数归零；安装到运行的同进程切换保留计数。抓取时读取内存计数和连接池快照，不访问数据库，因此故障期间仍可抓取。

## 抓取与观察

同机原生 Prometheus 的最小抓取配置（若其 Web UI 已占用 9090，将应用 `.env` 的 `BLOG_METRICS_PORT` 改为 9091，并同步 target）：

```yaml
scrape_configs:
  - job_name: blog
    scrape_interval: 15s
    static_configs:
      - targets: ['127.0.0.1:9090']
```

Prometheus 若运行于容器，`127.0.0.1` 指向它自己；将其接入受控 Compose 网络并抓取 `blog:9090`。本仓库交付可抓取的应用指标，不安装监控存储或告警服务。

示例查询：

```promql
# 5xx 每秒数量；探针失败也计入，必要时通过 route 区分。
sum(rate(blog_http_requests_total{status=~"5.."}[5m]))

# 5xx 比例（无流量时为 0）。
sum(rate(blog_http_requests_total{status=~"5.."}[5m]))
  / clamp_min(sum(rate(blog_http_requests_total[5m])), 0.001)

# 各路由 p95 响应头延迟。
histogram_quantile(0.95, sum by (le, route) (rate(blog_http_request_duration_seconds_bucket[5m])))

# 运行池占用比例；多实例部署按 instance 区分。
blog_database_pool_connections{state="in_use"}
  / ignoring(state) blog_database_pool_connections{state="max"}
```

CI 的 Compose 演练覆盖安装前后的探针、版本身份、JSON 请求关联、管理端口映射、数据库暂停后的两秒 readiness 失败、liveness 与指标继续响应，以及数据库恢复与容器替换。生产告警阈值、容量与 RPO/RTO 仍按[部署验收](product-roadmap.md)确定。
