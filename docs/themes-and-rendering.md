# 主题、MiniJinja 与模板数据函数

状态：M0 原型已验证同步-异步桥接；生产渲染现已接入公开只读模板函数与第二主题 `themes/paper`。当前函数子集及预算见 §2/§4；未实现的候选函数仍列在后续范围。模板引擎使用 MiniJinja。

## 1. 选型与边界

MiniJinja 支持通过 `Environment::add_function` 注册函数、关键字参数以及通过 `State` 访问渲染状态，适合构建面向主题作者的查询 API。Tera 同样支持注册函数；选择 MiniJinja 是扩展接口和运行时模型的取舍，不意味着 Tera 无法获取数据，也不预先宣称性能更高。

MiniJinja 类型只出现在 infrastructure；application 定义引擎无关的公开查询门面、展示 DTO 与渲染端口，infrastructure 为每次渲染创建独立作用域。主题只能调用注册的只读函数，不能获取仓储、数据库连接、SQL、任意 HTTP 客户端或管理 API。

模板函数由核心服务提供并版本化，主题负责调用，不允许主题上传新的 Rust 可执行函数。该机制属于已经确认的受限扩展范围。

每个主题包含 `theme.json`（`schema_version: 1`、`theme_api_version: 1`、名称及 `required_functions`）。启动时检查版本和必需函数，未知函数或不兼容版本直接拒绝加载。当前支持的函数集合固定为下表六项；清单只描述兼容性，不赋予主题新权限。

## 2. 主题 API 示例

以下函数由应用提供，不是 MiniJinja 内置函数。当前可用：

```jinja
{% set recent = get_posts(limit=5, tag="rust") %}
{% for post in recent.items %}
  <a href="{{ post.url }}">{{ post.title }}</a>
{% endfor %}

{% set categories = get_categories(limit=20) %}
{% for category in categories.items %}
  <a href="{{ category.url }}">{{ category.name }}</a>
{% endfor %}

<link rel="stylesheet" href="{{ asset_url(path='main.css') }}">
```

| 函数 | 输入与输出 | 约束 |
|---|---|---|
| `get_posts` | `limit`（默认 10，1–50）、可选 `tag` 或 `category`（互斥）→ `{items}` 文章摘要 | 仅公开已发布未删除文章；`url` 为根相对路径，按发布时间及 ID 稳定排序 |
| `get_post` | `slug` → 公开文章摘要与 `updated_at` 或 none | 草稿、私密、回收站与不存在同为 none；不提供正文源文 |
| `get_categories` / `get_tags` | `limit`（默认 20，1–50）→ `{items}` 目录 | 返回公开目录名称、slug、URL；目录可为空，不暴露非公开引用量 |
| `asset_url` | `path` → 带内容摘要查询串的主题资源 URL | 仅主题 assets 中已扫描的普通文件；不存在或越界路径报错 |
| `post_url` | `slug` → 根相对文章 URL | 校验 slug，按 UTF-8 百分号编码 |

未知参数和超限请求返回受控模板错误，不接受原始 SQL、任意字段选择或任意条件表达式。当前只提供首批摘要，不接受 cursor/sort；`get_navigation`、`get_public_authors` 与 cursor 分页是候选扩展，尚未注册。`themes/paper` 的侧栏使用目录函数，文章页用 `get_posts` 查同类公开文章、`get_post` 查公开更新时间，资源与文章链接通过宿主函数生成。

基础上下文仍提供 `site`、`seo` 与当前页面主体。路由预加载文章正文，模板函数补充侧栏、分类和相关文章，不强迫全部内容通过函数获取。

### 2.1 当前已交付的固定上下文与 SEO 元数据

模板仍拿到预取主体上下文，并可额外调用上述公开函数；两个主题都含 7 个模板：base、index、post、page、tag、category、series。`BLOG_THEME_DIR` 指定启动默认主题；服务启动时加载其同级目录下清单有效、目录名与 slug 一致的主题。具有 `settings.manage` 权限的管理员可在后台「站点设置」选择主题，选择保存到独立的 `settings.theme` 行（版本 CAS）；公开 HTML 每次请求读取该行，切换后无需重启。若已保存的主题后来不可用，公开页面暂用启动默认主题，后台显示原选择并允许重新保存。主题资源使用 `/assets/{theme_slug}/{path}?v={hash}`，旧页面的样式资源仍可读取。部署新主题文件需要重启服务以加载模板和挂载资源；多实例部署需在所有实例安装同一主题集。

| 变量 | 内容 | 可用模板 |
|---|---|---|
| `site` | `title`、`description`（数据库 site 行 > 环境变量 > 内置默认值，每次渲染解析） | 全部 |
| `seo` | `title`、`description`、`canonical_url`、`feed_url`、`og_type` | 全部（`base.html` 依赖） |
| `posts` | 首页文章卡片列表 | index |
| `post` / `page` / `tag` / `category` / `series` | 各自页面主体 | 对应模板 |

文章详情上下文（`post`）与系列页上下文（`series`）带 `cover_url`，文章卡片/详情带 `author_avatar_url`，`site` 带 `logo_url`：值都是应用层生成的 `/media/{id}` 站内地址（无对应图片时为 none），模板用 `| url` 过滤器输出到属性并自行决定渲染。这些只是地址，**匿名可读性由媒体库按内容公开状态（文章/页面/系列/账号）实时判定**，模板不参与也不得缓存该判定。

- **标题、描述、canonical 只有一处规则**（application 的 `seo` 模块）：详情页标题是「页面标题 - 站点标题」，首页只有站点标题；描述折叠为单行并截断到 160 字符，文章优先取摘要、缺失时回退站点描述；canonical 是绝对 URL，列表页第 2 页起自指 `?page=N`。模板不再各自拼 `<title>`，`base.html` 已无 `{% block title %}`。
- **站点公开地址**取可信配置 `BLOG_PUBLIC_BASE_URL`（装配期用 `url` crate 解析并校验绝对 http/https、无凭据、无查询与片段、**无路径前缀**），不从请求 Host 头推导。子路径部署当前不支持：模板中的 `/assets/...`、`/posts/...`、`/feed.xml` 等都是域名根相对路径，接受前缀只会产出半套带前缀的链接，因此配置阶段直接拒绝。建议使用独立域名，并在对外域名根路径部署；仅由反向代理改写入站路径无法解决根相对链接问题。Unicode slug 在 URL 中按百分号编码，因此 canonical/feed/sitemap 对同一内容给出一致的地址。
- URL 值用 `url` 过滤器输出：`{{ seo.canonical_url | url }}`。MiniJinja 的 HTML 自动转义会把 `/` 写成 `&#x2f;`（合法但让地址不可读、外部工具比对失配），该过滤器保留 `/` 并兜底转义 `&`、`<`、`>`、`"`、`'` 后标记为安全字符串。标题、描述等普通文本继续走自动转义。
- **机器可读输出不经过模板**：`/feed.xml`、`/sitemap.xml`、`/robots.txt` 由 application 层纯函数渲染。RSS/sitemap 是协议契约，不应随主题变化，也不该让主题作者有机会产出不合规范的 XML；XML 转义与控制字符处理在该层单独测试。

## 3. 调用链与层次

```text
公开页面用例（application）
  → ThemeRenderer 端口
    → MiniJinjaRenderer（infrastructure）
      → 已注册模板函数（infrastructure 参数转换）
        → ThemeDataProvider 查询门面（application）
          → PublishedContentQuery 等端口
            → PostgreSQL / 读缓存适配器（infrastructure）
```

该运行时回调不新增反向编译依赖：infrastructure 实现 application 的端口，也可以调用 application 定义的查询门面；application 不依赖 MiniJinja。server 装配各实现。查询门面禁止再次调用渲染器，避免递归渲染或等待自己占用的工作队列。

当前 `ThemeData` 只封装公开查询端口，不接受 Actor；`RenderScope` 在 infrastructure 内持有截止时间、查询/调用预算与请求级缓存。预览、语言、主题版本和可配置预算尚未接入，不借公开函数提供这些能力。引擎 `Value` 不进入 application 契约。

模板 Environment 按活动主题版本复用，不能把当前用户、预览权限或请求缓存捕获到共享全局闭包中。请求级函数闭包或只读 Object 可绑定独立 RenderScope，必须避免跨请求共享可变身份。模板参数和模板变量不能修改可信权限范围；不能依赖模板可覆盖的变量充当授权依据。

## 4. 同步渲染与异步数据库

MiniJinja 的常规渲染与注册函数接口是同步接口，SQLx future 不会自动被模板 await。当前实现采用“页面主体异步预取 + 函数按需受控查询”：

1. 用例异步读取页面主体与已知公共数据，准备请求级结果缓存。
2. 在异步侧获取有界渲染许可，将同步渲染送入阻塞工作池；不能每次请求无上限创建线程。
3. 模板函数先查请求缓存和符合权限范围的读缓存。
4. 缺失时，仅从该阻塞工作线程，通过基础设施桥接调用异步查询门面；可采用多线程 Tokio runtime 的 Handle 驱动有超时的查询 future，具体机制通过原型验证。
5. 查询完成后返回 DTO，转换为 MiniJinja Value 并继续渲染。

禁止在 Axum 异步执行线程内直接 `block_on`，不为每次查询新建 runtime，不在同步锁或未提交写事务内等待查询。同步函数调用会占用渲染线程并可能串行访问数据库，因此这是功能支持机制，不是吞吐量优化；热点页面应依靠预取、缓存与批量查询减少 miss。

已启动的 Tokio `spawn_blocking` 任务不能通过 abort 强制停止。当前实现以每次宿主查询剩余 deadline 的 `tokio::time::timeout` 取消等待中的 future；MiniJinja fuel=200,000、递归深度=100，渲染结果超过 1 MiB 则拒绝返回（这是渲染后检查，不是分配时的硬内存上限）。渲染许可由实际工作持有到退出，客户端提前离开不会释放仍占用线程的许可。数据库侧 `statement_timeout` 与对纯模板执行时间的独立硬截止仍可在部署层补强。

当前预算：单列表默认 10（目录 20）、最大 50；每次渲染最多 10 次独立数据查询与 64 次函数调用，函数查询总截止 500ms；渲染许可 16 个、最多等待 250ms。重复查询命中请求缓存仍计入函数调用预算，但不重复计入数据库查询次数。饱和时返回受控服务错误，不无限排队。

### 对照方案与验证顺序

可选对照是主题清单声明数据需求，由 application 异步预取，函数只读准备好的结果。它能移除渲染中的数据库等待，但动态参数、依赖前序结果的查询需要声明语言或功能限制；CPU、递归、输出预算与列表失效依然存在。

保留函数取数需求，先用 M0 原型比较两种实现，不把“参数空间大”当作无法预取的证明，也不把静态声明当作缓存失效已经解决。原型不通过时记录 ADR 调整桥接/API；正式主题编写不应建立在尚未验证的并发机制上。M0 可与采用纯预取的 M1 内容闭环独立推进，仅作为 M3 模板函数契约的门槛；原型位置及退出条件见 [路线图](product-roadmap.md)。

## 5. 权限、转义与错误

公开模板始终采用公开读取范围，即使请求者已登录管理员，也不自动暴露草稿。预览用例独立鉴权，仅允许读取被授权的目标内容；预览结果 no-store，默认关联列表仍为公开数据。请求缓存不得跨公开/预览共享。

只返回公开 DTO，不返回 User 聚合、会话、凭据或后台统计。模板不能通过 `status="draft"` 或伪造 actor_id 提权。列表、数量、关系数据与详情均采用一致可见性规则。

HTML 模板显式配置自动转义；启用严格未定义行为，并为可选字段提供明确默认值。正文先清洗，再由受控宿主标记为可安全插入 HTML；普通字符串不自动标安全。HTML 转义不能代替 JavaScript、CSS、URL 上下文的专门处理。主题仍仅由可信管理员安装，不宣称引擎提供完整恶意模板隔离。

`asset_url` 仅处理主题包内静态资源。封面、头像与站点 logo 均已接入媒体库：模板通过 `post.cover_url` / `series.cover_url` / `post.author_avatar_url` / `site.logo_url` 拿到 `/media/{id}` 地址。正文图片由 Markdown 渲染成 `<img src="/media/{id}">`，它与上述图片的匿名可读性由媒体库按 [内容生命周期 §5](content-lifecycle.md) 实时判定（文章撤回、账号软删除等都会立即失效），主题模板不参与也不得缓存该判定。分类/标签/系列 DTO 使用当前名称，关系及计数只来自当前 public、published、未软删除文章。

查询未找到返回 none/空集合；数据库失败、预算耗尽和参数错误不伪装成没有内容。关键渲染失败返回受控 5xx，服务端记录函数名、模板位置和脱敏错误。仅对明确声明可降级的可选区块使用回退，不缓存部分失败页面为正常页面。

## 6. 缓存依赖与版本

函数查询采用规范化查询键，包含查询种类、过滤/排序/分页参数、语言与读取范围。同一渲染去重，不允许以线程全局缓存保存私人上下文。

动态数据函数使页面依赖不再仅限主文章 ID。建议在 M3 引入页面缓存时使用站点公开内容 generation 作为粗粒度版本（M1 无页面缓存）：公开文章/页面保存、发布、撤回、归档、可见性、公开作者资料、分类/标签/系列变更均在事务中更新 generation。保存草稿不递增公开版本；当前没有“编辑已发布内容但不更新线上”的工作副本。页面键另包含主题、settings.version 和模板 API 版本；渲染前后核验版本，变化时不缓存混合结果。generation 的持久化协调在启用页面缓存时单独设计，不能将一个普通设置行的 version 自动视为全站版本。

空列表和未命中详情也具有查询依赖；新增文章必须使相应结果失效。异步通知与 TTL 用作补充，不能替代撤回后的可见性保证。缓存命中路径仍遵守架构文档中的即时撤回约束。

generation 的职责：application 定义 PublicContentVersion 读取与事务更新契约，发布用例在同一工作单元内协调递增，页面用例负责渲染前后核验；infrastructure 使用数据库原子更新和缓存适配器实现。它不属于 domain，也不由模板函数随意修改。并发更新同一记录会产生写争用，应测量并在事务冲突时受限重试，不能在应用进程读旧值再覆盖。

同一版本校验必须覆盖页面缓存和函数读缓存，不能使用新 generation 包装旧 DTO。建议初期从主库读取权威版本，禁止用滞后的本地版本缓存或读副本宣称立即撤回。公开可见性建议定义为：撤回事务完成后才开始的读取不能获得旧公开内容；已在执行中的读取可以按其一致性检查时点完成，已发送给客户端的字节无法追回。更严格的并发屏障是独立需求。版本变化时丢弃缓存写入；需要返回新视图时受限重读，超过重试预算返回受控错误。

该策略只约束经过应用的读取，不自动清除浏览器/CDN副本。要求撤回后新请求立即失效的正文与路径响应，必须强制重新验证或禁用共享缓存；若未来启用 CDN TTL，需明确有限陈旧窗口，不能继续承诺相同语义。

站点版本变化会使全站旧页面键不可命中，必须有容量和过期回收，避免旧键堆积。公开变更期间单独测命中率、整体 p95/p99、重建并发和数据库负载；热缓存命中延迟只是一个条件指标。细化依赖时，新内容进入列表和空查询转为非空也必须触发失效，不能只跟踪现有返回 ID。

以上方案的取舍见 [ADR-0004](adr/0004-public-cache-generation.md)。

模板 API 独立于主题版本和 MiniJinja 版本。清单声明兼容范围、所需函数与可选能力，激活时检查并使用样例数据渲染主要模板。任意分支无法靠一次样例渲染证明可用，运行时仍需受控报错与主题回退策略。升级引擎或函数契约运行主题兼容测试。

## 7. 验收

- 模板函数能按参数读取真实查询 DTO，并保持正确的分页与排序。
- 不同用户、并行预览和公开页面之间无请求状态串用。
- 重复调用不重复查询；循环内唯一查询超预算可控失败；批量 API 避免 N+1。
- 数据库慢查询、渲染超时与客户端取消不会无限占用工作线程或突破并发上限。
- 侧栏数据改变、空列表新增、文章撤回与主题切换使相关缓存失效。
- 转义、路径越界、缺失字段、非法参数、权限绕过均有针对性测试。
- 与静态上下文渲染比较冷/热缓存延迟及饱和吞吐量，校准预算后再承诺性能指标。

## 8. 参考

- [MiniJinja 函数](https://docs.rs/minijinja/latest/minijinja/functions/index.html)
- [Environment](https://docs.rs/minijinja/latest/minijinja/struct.Environment.html)
- [State](https://docs.rs/minijinja/latest/minijinja/struct.State.html)
- [Tera 函数能力](https://keats.github.io/tera/)
- [Tokio 同步桥接](https://tokio.rs/tokio/topics/bridging)
- [spawn_blocking 的取消限制](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)
