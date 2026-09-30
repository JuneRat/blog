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

服务和 CLI 的文本和 JSON 日志时间都读取环境变量 `TZ`，未设置时为 UTC，与后台站点时区独立；例如上海时间输出 `2026-09-28T21:26:12.044+08:00`，统一保留毫秒和显式偏移。初始化日志前会校验时区，错误配置不会先连接数据库。修改 `TZ` 需重启进程，空值或未知 IANA 名称会被拒绝。Docker Compose 在 `.env` 中设置 `TZ=Asia/Shanghai` 即可；原生启动可用 `TZ=Asia/Shanghai cargo run`。

文本模式省略 Rust 模块路径，请求完成记录在处理器 span 结束后输出，关联字段仅出现一次。例如：

```text
2026-09-28T21:26:12.044+08:00  INFO 请求完成 method=GET path=/ status=200 elapsed_ms=16 request_id=01a0e831-c93b-7532-9632-24c1257652d3 route=/
```

请求完成日志的 `fields` 含 `request_id`、`method`、`path`、`route`、`status`、`elapsed_ms`，认证成功后另有 `actor_id`。完成记录不重复输出请求 span；处理器内部的日志仍保留 span 上下文。响应头 `x-request-id`、管理错误体和日志中的编号保持一致，即使 `RUST_LOG=warn` 过滤了 info span，5xx 完成记录和管理内部错误仍携带编号。请求完成日志不包含查询参数、Cookie、授权头或请求正文；路径和已验证用户编号仍属于受控日志数据。5xx 记 warn，其余完成记录记 info；请求指标始终全量统计，不受日志过滤影响。

文本和 JSON 运行日志均进入 stderr，CLI 的机器可读结果仍写 stdout。安装码和启动地址是必须可见的运维提示，即使 `RUST_LOG=warn` 也会显示；两种模式下它们均使用相同时间格式写入 stderr。配置或日志初始化本身失败时，尚未建立日志器，错误仍是 stderr 文本。

预约发布检查即使没有到期内容也会访问数据库。失败记 ERROR「预约发布轮询失败，下次轮询重试」，含 `error`、`consecutive_failures`、本轮 `elapsed_ms` 和连接池 `pool_size/pool_idle/pool_max`；不表示一定有文章发布失败。失败后首次成功记 INFO「预约发布轮询已恢复」，含此前连续失败次数 `failed_polls` 和该成功轮询实际发布数量 `published_count`（可以是 0），随后重置失败计数。正常连续成功不打印轮询日志；INFO 恢复记录受 `RUST_LOG` 过滤，轮询指标仍全量统计。

连接池字段只读取本地计数，不额外申请数据库连接，是轮询结束时的瞬时快照，不能单凭它确定历史超时原因。连接获取超时也涵盖旧连接检查和新连接建立；本机睡眠或容器短暂不可用也可能触发。任务仍按原有 30 秒间隔轮询，失败批次留待重试。

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
| `blog_render_waiting{kind}` | 等待渲染许可的任务数 |
| `blog_render_active{kind}` | 实际执行中的阻塞渲染任务数；调用者已超时的工作仍计入，直到实际退出 |
| `blog_render_queue_duration_seconds{kind}` | 等待渲染许可的耗时，包含取消和未获得许可的等待 |
| `blog_render_queue_total{kind,result}` | 已结束的许可等待次数，结果为 `admitted/timeout/cancelled/closed` |
| `blog_render_execution_duration_seconds{kind}` | 实际 worker 执行耗时，不含阻塞池调度等待 |
| `blog_render_completed_total{kind,result}` | 已退出的渲染任务数，结果为 `success/error` |
| `blog_render_execution_timeouts_total{kind}` | 调用者等待渲染结果超时次数；不表示 worker 已被终止 |
| `blog_scheduled_publication_runs_total{result}` | 预约发布轮询完成次数，结果为 `success/error` |
| `blog_scheduled_publication_run_duration_seconds` | 每次预约轮询处理耗时 |
| `blog_scheduled_publication_last_success_timestamp_seconds` | 最近成功轮询的 Unix 时间；首次成功前为 0 |

标签使用路由模板，如 `/api/admin/posts/{id}`，未知路径统一为 `unmatched`，扩展 HTTP 方法统一为 `OTHER`。不将 slug、用户 ID、请求编号或查询参数放入指标标签。每次进程重启计数归零；安装到运行的同进程切换保留计数。抓取时读取内存计数和连接池快照，不访问数据库，因此故障期间仍可抓取。

渲染 `kind` 固定为 `content/comment/theme`。Markdown 缓存命中不启动 worker，因此不增加执行次数；等待许可期间取消计入队列的 `cancelled` 结果。预约指标描述服务中的轮询健康与耗时，不是从预约时间到实际发布的延迟；停机期间到期内容仍按[内容生命周期](content-lifecycle.md)补发，精确发布时间偏差需另行测量。

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

# 各类渲染的 p95 许可等待时间。
histogram_quantile(0.95, sum by (le, kind) (rate(blog_render_queue_duration_seconds_bucket[5m])))

# 距上次成功预约轮询的秒数；0 值须单独识别为尚未成功。
time() - blog_scheduled_publication_last_success_timestamp_seconds
```

CI 的 Compose 演练覆盖安装前后的探针、版本身份、JSON 请求关联、管理端口映射、数据库暂停后的两秒 readiness 失败、liveness 与指标继续响应，以及数据库恢复与容器替换。生产告警阈值、容量与 RPO/RTO 仍按[部署验收](product-roadmap.md)确定。

每次候选部署在[上线验收记录](release-acceptance-template.md)填写告警阈值、持续时间、接收人和恢复时间目标，关联实际触发与恢复证据。混合压测的客户端延迟包含响应体及写入前的版本读取；上方 HTTP 直方图只统计单次请求到响应头的耗时，二者应分别展示。

## 站点设置读取降级

公开页面继续在站点设置读取失败时使用装配回退值；未设置与读取失败分别观察，不以页面仍返回 200 推断配置正常。存储适配器首次失败记 WARN，持续失败每分钟至多重复一次，后续首次成功记 INFO 恢复；日志和指标不包含设置正文或数据库错误字符串。

| 指标 | 含义 |
|---|---|
| `blog_site_settings_reads_total{result}` | `configured`、`missing` 或 `error`，包含公开回退隐藏的读取失败 |
| `blog_site_settings_read_available` | 最近一次读取成功为 1、失败为 0、尚未读取为 -1；不是整体数据库健康检查 |
| `blog_site_settings_read_recoveries_total` | 连续失败后首次成功的次数 |

告警结合 `increase(blog_site_settings_reads_total{result="error"}[5m])` 与最近读取状态；设置行不存在是合法初始状态，计入 `missing`，不会标为降级。
