# 主题与渲染

公开站点使用 MiniJinja SSR，已提供 [default](../themes/default/) 和 [paper](../themes/paper/) 两个主题。应用层定义引擎无关的异步渲染端口与公开数据 DTO；基础设施负责模板、Markdown 清洗、媒体引用提取、执行预算与缓存。后台 SPA 不使用这些主题。

[目标数据库设计](database-design.md)已确认评论 HTML 持久化、系列多对多、定时公开条件及媒体链接独立公开，主题 DTO、评论组件和读取条件尚待同步。本文仍描述当前主题契约；新评论只能输出服务端清洗且版本匹配的 HTML，不能直接把现有纯文本字段改作 HTML 输出。

## 主题包与加载

主题是管理员部署的可信文件，不是可上传执行任意代码的插件。当前目录结构：

```text
themes/default/
├── theme.json
├── templates/
│   ├── base.html
│   ├── index.html
│   ├── post.html
│   ├── page.html
│   ├── tag.html
│   ├── category.html
│   └── series.html
└── assets/
    └── style.css
```

六个页面入口（index/post/page/tag/category/series）是必需文件。`base.html` 是可选辅助模板；`templates/` 递归加载，包括 `partials/`、宏等辅助文件，禁止符号链接。所有模板在启动时编译并持有源码，不在请求中读取磁盘。清单只接受以下字段；两个版本字段当前都必须为 `1`：

```json
{
  "schema_version": 1,
  "slug": "default",
  "name": "Default",
  "theme_api_version": 1,
  "required_functions": []
}
```

`slug` 只允许小写 ASCII 字母、数字、`-`，主题目录名必须与它一致，名称不能为空。`required_functions` 只能包含已注册函数；它声明所需能力，不授予额外权限。清单与模板校验在 [rendering.rs](../crates/infrastructure/src/rendering.rs)，加载和资源挂载在 [website.rs](../crates/server/src/website.rs)。

`BLOG_THEME_DIR` 指定启动默认主题。启动时加载它及同级目录下有效的主题，解析模板并读取静态资源快照；默认主题失败会阻止 `serve`，其他无效主题被跳过。通过统一的 `MiniJinjaThemeRenderer::load_checked` 入口，使用固定公开数据和真实执行器验证六类页面，覆盖空列表、缺少可选字段、50 篇首页、20 篇目录页及分页首/中/末页。校验沿用函数参数、查询和执行预算，不访问数据库；报错包含场景和页面入口。它是发布前契约检查，不穷尽任意数据相关分支。只有通过检查的快照进入可选主题注册表。请求复用已加载的模板与资源字节，不读取磁盘。新增或修改主题文件后需重启，使模板和资源快照一同生效；没有热安装、后台上传或样例预览发布流程。

后台站点设置可以选择已加载主题，选择以版本条件保存到 `settings.theme`。公开请求每次读取活动选择，切换已安装主题无需重启；未配置或对应主题不在启动注册表中时，使用启动默认主题。设置查询或模板执行失败会返回错误，不触发这项回退。资源按 `/assets/{theme_slug}/{release}/{path}` 分开，release 为模板、资源路径及字节内容共同计算的完整 SHA-256。HTTP 仅返回同一快照中的资源字节，并为成功响应设置一年 immutable 缓存；不存在的版本或文件返回 404。进程内原地替换磁盘文件不会改变既有 URL 的内容。重启后旧版本若未继续部署则返回 404，不会返回新内容；需要跨版本保留资源时应在部署层保留旧版本或使用 CDN。快照占用的内存与全部已加载主题资源大小有关。

配置项见[配置参考](configuration.md)，设置权限与版本契约见[管理 API](admin-api.md)。

## 固定上下文

[公开用例](../crates/application/src/public_site.rs) 先读取页面主体，再交给活动主题。每个页面都有 `site` 和 `seo`：

| 变量 | 当前内容 | 模板 |
|---|---|---|
| `site` | `title`、`description`、可空 `logo_url` | 全部 |
| `seo` | `title`、`description`、`canonical_url`、`feed_url`、`og_type` | 全部 |
| `posts` | 文章卡片列表：标题、slug、`url`、摘要、发布时间、作者展示名及可空头像 URL | index |
| `post` | 文章详情、清洗后 `content_html`、标签、系列数组、可空分类/封面 | post |
| `page` | 页面详情与清洗后 `content_html` | page |
| `tag` / `category` | 目录名称、slug、页码、总页数和文章卡片 | tag / category |
| `series` | 系列名称、slug、可空封面、分页和带连续阅读序号的文章卡片 | series |

`post.series` 为数组，每项包含 `slug`、`name` 和排序权重 `position`，无关联时为空数组。链接使用 `/series/{slug}`，权重可重复，不是章节编号；系列页文章卡片另提供连续阅读序号。

可空图片 URL 为 `/media/{id}`。文章卡片和详情有 `author_avatar_url`，详情有 `cover_url`；已登记媒体链接独立公开，不随文章隐私、引用变化或媒体软删除撤销读取。

站点信息每次按数据库设置、装配回退值解析；SEO 规则集中在 [seo.rs](../crates/application/src/seo.rs)，主题只输出结果。canonical、RSS 和 sitemap 使用经过验证的 `BLOG_PUBLIC_BASE_URL`，不取请求 Host；当前要求部署在域名根路径，不支持 URL 路径前缀。描述折叠为空白单行并限制为 160 字符，目录分页从第 2 页起使用自指 canonical。

### RSS、sitemap 与 robots

机器可读输出由应用层[协议渲染函数](../crates/application/src/syndication.rs)和公开用例生成，不经过主题，主题切换不改变其协议契约。

| 路径 | 当前契约 |
|---|---|
| `/feed.xml` | RSS 2.0；按发布时间及 ID 倒序取最新 20 篇公开文章；输出摘要而非正文，文章绝对 URL 同时作为 link 和稳定 guid |
| `/sitemap.xml` | 收录首页、公开文章、公开 Page，以及至少有一篇公开文章的标签/分类/系列目录；目录只收录第 1 页；内容条目 lastmod 使用更新时间 |
| `/robots.txt` | 允许抓取公开内容，排除 `/admin`、`/api`、`/auth`，并声明绝对 sitemap 地址 |

sitemap 的 50,000 条限制是整个文件的预算：首页、文章、Page、目录共用，按此顺序分配；超出部分截断，当前没有 sitemap index。XML 文本统一转义并排除非法控制字符；Unicode slug 的路径编码与 canonical 保持一致。三个响应均设置 `Cache-Control: no-cache`，当前不生成 ETag。

## 模板数据函数

当前注册六个只读函数。参数采用关键字形式，未知参数、非法 slug 和超限输入都报模板错误。

| 函数 | 输入 | 输出与边界 |
|---|---|---|
| `get_posts` | `limit=10`，范围 1–50；可选 `tag` 或 `category`，二者互斥 | `{items}` 公开文章摘要，按发布时间及 ID 倒序；只取首批 |
| `get_post` | `slug` | 公开文章摘要加 `updated_at`，不可见或不存在返回 none；不返回正文 |
| `get_categories` | `limit=20`，范围 1–50 | `{items}` 分类名称、slug 和 URL |
| `get_tags` | `limit=20`，范围 1–50 | `{items}` 标签名称、slug 和 URL |
| `asset_url` | `path` | 当前主题已扫描资源的带指纹 URL；不存在或越界报错 |
| `post_url` | `slug` | 校验并按 UTF-8 百分号编码后的根相对文章 URL |

文章摘要字段为 `title`、`slug`、`url`、`excerpt`、`published_at`、`author_display`、`author_avatar_url`，与固定上下文 `PostCard` 使用同一个类型。详情上下文也提供 `url`；已有的 `updated_at` 可直接读取，无需再调用 `get_post`。目录可以没有公开文章，不透出非公开引用量。函数没有 cursor、排序选择、任意字段查询或 SQL 参数。

```jinja
{% set recent = get_posts(limit=5, tag="rust") %}
{% for item in recent.items %}
  <a href="{{ item.url | url }}">{{ item.title }}</a>
{% endfor %}
<link rel="stylesheet" href="{{ asset_url(path='style.css') | url }}">
```

[ThemeData](../crates/application/src/theme_data.rs) 只依赖公开查询端口，不接受 Actor，也不会因为访问者已登录而返回草稿或私密内容。每次渲染创建独立的 [RenderScope](../crates/infrastructure/src/theme_functions.rs)，持有函数预算、截止时间和请求内查询缓存；共享模板环境不保存用户或请求状态。当前没有服务端主题预览权限范围。

## 正文渲染与持久化

[`ContentRenderer::render_content`](../crates/application/src/ports/rendering.rs) 返回 `RenderedContent { content_html, media_ids }`。生产实现把以下工作放入同一个受控阻塞任务：

1. 用 pulldown-cmark 转换 Markdown，启用表格与删除线。
2. 用 ammonia 清洗 HTML。
3. 从这份清洗结果提取 `<img src="/media/{uuid}">`，按 UUID 排序、去重。

注释、被清洗掉的标签、普通链接和纯文本不形成图片引用。独立封面由保存侧并入引用集合；它不属于正文渲染结果。媒体引用提取不会在异步数据库线程上再次解析 HTML。

[内容仓储](../crates/infrastructure/src/persistence/content.rs) 在事务外等待渲染，然后把源文、`content_html`、`content_render_version` 和媒体关系同事务提交。公开 Post/Page 详情直接读取保存的 HTML，不在每次访问时转换 Markdown。

清洗规则改变时需要递增 `CONTENT_RENDER_VERSION`。完整迁移入口分批重建不匹配的记录，以业务版本和源文作条件，避免覆盖并发编辑；HTML 与引用一起更新，不增加编辑版本或改变业务更新时间。结构迁移与完整迁移的命令分工见[架构](architecture.md)。

## 执行策略与实际预算

[`RenderingRuntime`](../crates/infrastructure/src/render_executor.rs) 为正文写入和公开主题分配独立的许可池：正文默认 4 个，全部主题共用 16 个。主题查询等待不会占用正文许可。两者仍共用 Tokio 阻塞线程池，许可隔离并非独立 CPU 或线程池。应用只调用异步端口，不持有 Tokio 信号量，也不自行使用 `spawn_blocking` 或超时。

主题主体先异步预取；同步 MiniJinja 渲染在阻塞池内运行。模板函数缺少请求缓存时，才从该阻塞线程通过 Tokio Handle 驱动带剩余截止时间的公开查询。桥接不会新建 runtime，也不在异步工作线程中 `block_on`；每次查询等待会占用当前渲染许可。

| 限制 | 当前默认值 | 作用范围 |
|---|---|---|
| 主题渲染并发 | 16 | 全部公开主题共享 |
| 正文渲染并发 | 4 | 正文转换与引用提取，独立于主题许可 |
| 等待许可 | 250ms | 超时返回渲染错误；没有另设排队人数上限 |
| 等待执行结果 | 2s | 从提交阻塞任务开始计时，包含阻塞池调度等待 |
| MiniJinja fuel | 200,000 | 每次模板渲染 |
| 模板递归深度 | 100 | MiniJinja 环境 |
| 主题输出 | 1 MiB | 渲染完成后的长度检查，不是硬内存上限 |
| 数据函数调用 | 64 | 每次渲染，查询缓存命中也计数；`asset_url` / `post_url` 仅受 fuel 和截止时间限制 |
| 独立数据查询 | 10 | 每次渲染，重复查询命中缓存不再计数 |
| 函数截止时间 | 500ms | 从 RenderScope 创建起；查询按剩余时间等待 |
| Markdown LRU | 64 条、8 MiB | 源文、HTML 和媒体 UUID 合计，两个上限同时生效 |

当前服务使用默认值；`RenderingLimits` 可在 Rust 装配时设置，不是已开放的环境变量配置。过载、模板错误和超时返回受控渲染错误，不伪装成空数据。

已启动的阻塞任务不能被强制终止。2s 超时或调用者取消会结束等待，但许可留在实际工作中直到退出；尚未启动的任务在执行超时时尝试取消。fuel、递归限制和函数查询 deadline 提供额外约束，但不构成恶意模板安全沙箱或硬 CPU/内存隔离。

缓存只保存由完整源文决定的 HTML 与正文引用，不缓存主题页面、数据库查询结果或媒体权限。单条结果超出容量时正常返回但不入缓存；命中不占渲染许可。tracing 记录命中、输入大小、排队和执行耗时、成功状态及媒体数量，不记录正文内容。请求内函数缓存随渲染结束释放。

## 转义与公开性

所有主题模板统一采用 MiniJinja 的 HTML 自动转义和严格未定义模式，不按文件扩展名切换转义策略；通过 `include`、继承或宏复用的 `.jinja`、`.j2` 及无扩展名辅助模板也使用 HTML 自动转义。清洗后的 `post.content_html` / `page.content_html` 可使用 `|safe`；普通标题、描述和用户资料保持自动转义。`url` 过滤器保留 `/` 并转义 HTML 属性特殊字符，供宿主生成的地址使用；它不替代 JavaScript、CSS 或任意 URL 的专门校验。

公开查询只允许已发布、公开、未删除的文章及已发布、公开的 Page。缺失内容可以返回 none 或 404，数据库失败、参数错误、预算耗尽和模板错误则保留为错误。当前没有公开页面缓存、跨请求函数缓存、全站 generation 或主题失败页面缓存；主题资源版本与内容公开性分别管理。

关于页面缓存、模板 API 扩展及外部集成的后续取舍，见[产品路线图](product-roadmap.md)与[扩展设计](extensions-and-data.md)。历史桥接、缓存方案及执行边界决策保留在 [ADR-0002](adr/0002-template-data-functions.md)、[ADR-0004](adr/0004-public-cache-generation.md)、[ADR-0015](adr/0015-rendered-content-runtime-and-module-boundaries.md)；其中规划方案不等于当前实现。

## 文章评论组件

内置 default / paper 的文章模板通过 `data-comments-slug="{{ post.slug }}"` 挂载原生评论，加载 `/assets/comments.js` 与 `/assets/comments.css`。这些共享资源由 Rust 提供，文本使用 DOM `textContent`；不得改为正文的 `safe` 输出。公开列表和提交开关由同源 API 实时检查文章可见性。行为、分页及接口见[评论](comments.md)。
