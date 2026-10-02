# 主题与渲染

公开站点使用 MiniJinja SSR，内置 [Default](../themes/default/)，[Paper](../theme-packages/paper/) 以[第三方 ZIP 主题包](../theme-packages/README.md#paper)形式提供，需在「主题管理」上传安装。应用层定义引擎无关的异步渲染端口与公开数据 DTO；基础设施负责模板、Markdown 清洗、媒体引用提取、执行预算与缓存。后台 SPA 不使用这些主题。

[数据库设计](database-design.md)中的评论 HTML 持久化、系列多对多、定时公开条件及媒体链接独立公开均已接入主题读取。评论只能输出服务端清洗的 content_html，昵称和占位继续作为文本显示。

## 主题包与加载

主题是持有 `settings.manage` 权限的管理员安装的可信模板与静态资源。主题包不会注册服务端可执行代码；模板与浏览器资源仍需由管理员信任。当前目录结构：

```text
themes/default/
├── theme.json
├── settings.schema.json  # 可选配置声明
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

`slug` 只允许小写 ASCII 字母、数字、`-`，主题目录名必须与它一致，名称不能为空。`required_functions` 只能包含已注册函数；它声明所需能力，不授予额外权限。清单与模板校验在 [rendering.rs](../crates/infrastructure/src/rendering.rs)，安装、卸载与启动加载在 [theme_packages.rs](../crates/infrastructure/src/theme_packages.rs)，服务装配在 [website.rs](../crates/server/src/website.rs)。

`BLOG_THEME_DIR` 指定启动默认主题。启动时加载它及同级目录下有效的主题，解析模板并读取静态资源快照；默认主题失败会阻止 `serve`，其他无效主题被跳过。加载和安装均使用固定公开数据和真实执行器验证六类页面，覆盖空列表、缺少可选字段、首页配置允许的最大容量、目录页及分页首/中/末页。校验沿用函数参数、查询和执行预算，不访问数据库；报错包含场景和页面入口。它是发布前契约检查，不穷尽任意数据相关分支。只有通过检查的快照进入可选主题注册表。请求复用已加载的模板与资源字节，不读取磁盘。后台「主题管理」（`/admin/themes`，与插件管理并列）支持上传 ZIP、验证、安装、激活和卸载，新安装的主题无需重启即可选用。手工修改磁盘上的已安装文件仍需重启；「验证」检查正在使用的已安装快照，不重新加载磁盘改动。当前没有主题样例预览发布流程。

后台主题管理可以选择已安装且验证通过的主题，选择以版本条件保存到 `settings.theme`。公开请求每次读取活动选择，切换已安装主题无需重启；未配置或对应主题不在启动注册表中时，使用启动默认主题。设置查询或模板执行失败会返回错误，不触发这项回退。资源按 `/assets/{theme_slug}/{release}/{path}` 分开，release 为模板、资源和规范化配置声明共同计算的完整 SHA-256。HTTP 仅返回同一快照中的资源字节，并为成功响应设置一年 immutable 缓存；不存在的版本或文件返回 404。进程内原地替换磁盘文件不会改变既有 URL 的内容。卸载后，该主题的资源 URL 返回 404；重启后旧版本若未继续部署也返回 404，不会返回新内容；需要跨版本保留资源时应在部署层保留旧版本或使用 CDN。快照占用的内存与全部已加载主题资源大小有关。

### 安装、验证、激活和卸载

ZIP 可以直接包含 `theme.json`、`templates/` 和 `assets/`，或包含一个顶层主题目录。包中必须只有一份清单，安装目录使用清单 slug；可在本地执行 `cd themes && zip -r /tmp/my-theme.zip my-theme` 打包。限制为压缩包 10 MiB、最多 512 个文件或目录、单文件 4 MiB、解压总计 32 MiB、路径 240 字节、目录深度 16 层。仅支持 Stored/Deflate，不接受加密条目、符号链接、特殊文件、绝对路径、点路径或反斜杠。清单最多 16 KiB；slug 最多 64 个 ASCII 字符。

验证上传包会在同级隐藏暂存目录解压，执行清单/API 检查、模板编译和六类页面契约预检，返回 slug、名称、release、模板与资源数量；操作结束清理暂存，不登记、不改变选择。安装执行同样的检查，通过后将暂存目录原子重命名并发布模板与资源快照；重复 slug、已存在目录和超过 32 个已安装主题均拒绝，不覆盖现有主题。失败保持已有注册表与主题文件。暂存和删除残留以隐藏目录命名，启动时不会作为主题加载。

激活继续通过 `settings.theme` 的版本条件保存；安装与卸载不递增该选择版本。默认主题和当前生效/已保存选择的主题不能卸载。卸载同时携带选择的 `expected_version`、列表快照中的主题 `id`、`expected_release`、`config_schema_version`、`expected_config_version`；任何身份或版本变化返回 409。同名同版本重装会获得新 UUID，旧页面无法保存配置或卸载新的安装。

安装先预检和暂存全部字节，再持久化 `.theme-op-<uuid>/operation.json`，原子发布目录，提交 `themes` 记录和审计，最后发布内存快照。卸载先写操作日志，将目录移到日志的 `payload/` 隔离区并退出资源注册表，再同事务删除配置记录、媒体引用和写审计。失败先等待原主题事务释放数据库锁，再依据数据库中的主题 UUID 判断补偿：未提交安装回收目录，未提交卸载恢复目录及快照；提交结果不确定时保留日志、关闭后续包变更，须恢复数据库连接后重启正常服务。正常启动在加载主题前恢复日志，已提交卸载只收集隔离目录；目录缺失、加载失败或日志身份不一致均不清空配置。垃圾收集失败保留日志并报错。请求取消不会中断拥有操作锁的后台提交任务；进程退出由日志恢复。主题媒体文件不随卸载删除，重新安装只使用默认配置。

正常服务持有 `.theme-owner.lock` 文件租约，拒绝多个服务实例共同管理同一目录；激活、配置保存、安装及卸载还共享 PostgreSQL `THEMES` 事务锁。主题机制仍面向单个 `serve` 实例。运行目录须可写，主题文件与数据库一起备份；存在未恢复操作日志时备份工具拒绝备份，先正常启动完成恢复。Compose 的主题命名卷保留已安装包。恢复隔离装配只读配置和主题，不补齐记录、不恢复文件操作；安装、卸载、激活及保存继续受会话、CSRF、`settings.manage` 和隔离规则保护。安装、卸载及配置保存均有同事务审计。

### 主题配置声明与表单

首次安装先预检主题文件，不写入主题记录；账号与安装标记事务成功后，才初始化 `themes` 并开放网站。主题初始化失败会保留已提交的安装标记供重试，重启补齐记录不会重复创建账号。带有未完成主题操作日志的目录不能用于新站点安装，须先恢复原站点。

主题根目录可选提供 `settings.schema.json`，无声明的旧包使用版本 1 的空字段列表和空配置。名称、字段定义和默认值全部来自包；`themes` 每个 slug 一条 UUID 记录，保存覆盖值、配置结构版本、编辑版本及发布标识。正常启动为有效的已有磁盘主题幂等补齐记录。兼容的新声明可保留覆盖值并更新发布/结构版本，编辑版本递增；未知旧字段、类型或约束不兼容会明确报错并保留原记录，默认主题不兼容会阻止启动，其他主题退出可用列表。首版没有升级脚本；修改包前应先在旧声明下清理不再适用的覆盖值。

```json
{
  "config_schema_version": 1,
  "fields": [
    {"key":"accent_color","type":"color","label":"主题色","group":"外观","default":"#2563eb"},
    {"key":"show_toc","type":"boolean","label":"显示目录","default":true},
    {"key":"header_image","type":"media","label":"页眉图片","default":null}
  ]
}
```

声明与覆盖值各不超过 64 KiB，最多 64 个字段；结构版本为 1..2147483647。字段键使用 `[a-z][a-z0-9_]{0,63}`，不允许重复；`label` 必填、最多 100 字符，`description` 最多 1000 字符，`group` 最多 100 字符。`default` 必须提供。未知声明属性和未声明配置键被拒绝。支持：

| type | 值和约束 |
|---|---|
| `text` / `textarea` | 字符串；`min_length`、`max_length` 按 Unicode 码点，默认范围 0..8192 |
| `integer` | JS 安全整数；可选 `min`、`max` |
| `boolean` | JSON `true` / `false` |
| `select` | 字符串，来自 `options: [{value,label}]`；1..64 个唯一选项，value 最多 200 字符 |
| `color` | `#RRGGBB`，十六进制大小写均可 |
| `media` | 规范化的非 nil 媒体 UUID 字符串或 `null`；默认只能为 `null` |

长度约束只用于文本，范围约束只用于整数，选项只用于单选。缺省键使用默认值；显式空字符串、false 和媒体 null 保留，不进行递归合并。后台「配置」支持已安装的非活动主题，按分组生成控件、显示说明和字段错误，复用媒体选择器；「恢复默认值」删除对应覆盖键。冲突保留当前输入，重新加载服务器身份/配置后继续编辑，不自动重试。

`GET /api/admin/v1/themes/{slug}/settings` 返回 `id`、`slug`、`release`、`fields`、生效 `config`、原始 `overrides`、`config_schema_version` 和 `version`。`PUT` 提交覆盖对象 `config`、`id`、`expected_release`、`config_schema_version`、`expected_version`，独立校验身份、版本、字段和新媒体可引用性；同版本同覆盖值幂等，不增版本也不写审计。修改配置、同步 `media_refs` 和审计同事务提交。`media_refs.source_type='theme'` 使用 `themes.id`，持久化的 `media_fields` 只记录媒体字段名，供恢复及物理清理在不执行主题代码时核验 UUID 引用；媒体使用位置只对 `settings.manage` 持有者显示。

每次请求先选定最终主题，再读取匹配发布的配置，注入独立的 `theme` 上下文：

```jinja
{{ theme.config.accent_color }}
{% if theme.config.show_toc %} ... {% endif %}
{% if theme.config.header_image %}<img src="{{ ('/media/' ~ theme.config.header_image) | url }}" alt="">{% endif %}
```

默认主题回退使用默认主题自己的覆盖值，配置不会写入共享模板环境。预检仅使用声明默认值和固定数据，没有数据库依赖。Default 已接入主题色、描述开关、页脚文字和页眉图片示例。

配置项见[配置参考](configuration.md)，设置权限与版本契约见[管理 API](admin-api.md)。

## 固定上下文

[公开用例](../crates/application/src/public_site.rs) 先读取页面主体，再交给活动主题。每个页面都有 `site` 和 `seo`：

| 变量 | 当前内容 | 模板 |
|---|---|---|
| `site` | `title`、`description`、可空 `logo_url` | 全部 |
| `seo` | `title`、`description`、`canonical_url`、`feed_url`、`og_type` | 全部 |
| `pagination` | 首页 `page`、可空 `previous_url/next_url`，由应用层生成 | index |
| `site.home_page_size` | 首页每页文章数 | 全部主题页面 |
| `site.navigation` | 当前公开页面导航数组：`label/url/placement`，保持配置顺序 | 全部主题页面 |
| `posts` | 文章卡片列表：标题、slug、`url`、摘要、发布时间、作者展示名及可空头像 URL | index |
| `post` | 文章详情、清洗后 `content_html`、标签、系列数组、可空分类/封面 | post |
| `page` | 页面详情与清洗后 `content_html` | page |
| `tag` / `category` | 目录名称、slug、页码、总页数和文章卡片 | tag / category |
| `series` | 系列名称、slug、可空封面、分页和带连续阅读序号的文章卡片 | series |

`post.series` 为数组，每项包含 `slug`、`name` 和排序权重 `position`，无关联时为空数组。链接使用 `/series/{slug}`，权重可重复，不是章节编号；系列页文章卡片另提供连续阅读序号。

可空图片 URL 为 `/media/{id}`。文章卡片和详情有 `author_avatar_url`，详情有 `cover_url`；已登记媒体链接独立公开，不随文章隐私、引用变化或媒体软删除撤销读取。

站点信息每次按数据库设置、装配回退值解析；SEO 规则集中在 [seo.rs](../crates/application/src/seo.rs)，主题只输出结果。canonical、RSS 和 sitemap 使用经过验证的 `BLOG_PUBLIC_BASE_URL`，不取请求 Host；当前要求部署在域名根路径，不支持 URL 路径前缀。描述折叠为空白单行并限制为 160 字符，目录分页从第 2 页起使用自指 canonical。

### 首页分页与页面导航

首页 `/` 每页数量由站点设置 `home_page_size` 决定，默认 20，可在后台设置为 1–100，保存后下次请求即生效；后续页为 `/?page=N`，按发布时间和 ID 倒序。用多取一条判断下一页，模板使用 `pagination.previous_url/next_url`；第一页 canonical 为 `/`，后续页自指，越过末页返回 404，非法页码格式或溢出返回 400，非正数沿用目录分页规范化为第一页。空站点仍返回第一页。跨请求翻页不冻结数据集合。

后台站点设置配置独立页面导航名称、目标 slug、页头/页脚位置及顺序。每次主题渲染以一次批量查询复核公开条件，只向模板提供可公开页面的链接；页面撤回、预约、私有、回收站或物理删除均隐藏入口，重新公开后恢复。不输出隐藏页面的名称或 slug。导航按路径配置，草稿改 slug 后需同步调整；永久删除后若新页面复用相同 slug，导航会指向该新页面。default/paper 均实现页头与页脚导航；导航是 `site` 上下文字段，不是可执行模板函数。

### RSS、sitemap 与 robots

公开用例返回 [RSS 频道与 sitemap 条目](../crates/application/src/syndication.rs)，接口层负责 [XML 转义、日期格式与 robots 编码](../crates/interfaces/src/syndication.rs)以及 HTTP 响应。数据查询与协议编码均不经过主题，主题切换不改变其协议契约。

| 路径 | 当前契约 |
|---|---|
| `/feed.xml` | RSS 2.0；按发布时间及 ID 倒序取最新 20 篇公开文章；输出摘要而非正文，文章绝对 URL 同时作为 link 和稳定 guid |
| `/sitemap.xml` | 收录首页、公开文章、公开 Page，以及至少有一篇公开文章的标签/分类/系列目录；目录只收录第 1 页；内容条目 lastmod 使用更新时间 |
| `/robots.txt` | 允许抓取公开内容，排除 `/admin`、`/api`、`/auth`，并声明绝对 sitemap 地址 |

sitemap 的 50,000 条限制是整个文件的预算：首页、文章、Page、标签、分类、系列共用，按此顺序分配。每个来源的 SQL 查询都以剩余额度为上限，额度耗尽后跳过后续来源；应用用例保证返回条目不超限，接口编码另有兜底截断。当前没有 sitemap index。XML 文本统一转义并排除非法控制字符；Unicode slug 的路径编码与 canonical 保持一致。三个响应均设置 `Cache-Control: no-cache`，当前不生成 ETag。

## 模板数据函数

当前提供以下只读数据函数和插件资源函数。数据参数采用关键字形式，未知参数、非法 slug 和超限输入都报模板错误。

| 函数 | 输入 | 输出与边界 |
|---|---|---|
| `get_posts` | `limit=10`，范围 1–50；可选 `tag` 或 `category`，二者互斥 | `{items}` 公开文章摘要，按发布时间及 ID 倒序；只取首批 |
| `get_post` | `slug` | 公开文章摘要加 `updated_at`，不可见或不存在返回 none；不返回正文 |
| `get_categories` | `limit=20`，范围 1–50 | `{items}` 分类名称、slug 和 URL |
| `get_tags` | `limit=20`，范围 1–50 | `{items}` 标签名称、slug 和 URL |
| `asset_url` | `path` | 当前主题已扫描资源的带指纹 URL；不存在或越界报错 |
| `post_url` | `slug` | 校验并按 UTF-8 百分号编码后的根相对文章 URL |
| `plugin_head` | 无参数 | 本次页面已启用插件的 CSS 和带 `defer` 的 JS 标签；在 `<head>` 内调用一次，无需 `safe` |

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

[`ContentRenderer::render_content`](../crates/application/src/ports/rendering.rs) 返回 `RenderedContent { content_html, media_ids, render_version }`。生产实现读取已启用插件快照，再把以下工作放入同一个受控阻塞任务：

1. 执行正文插件的源文/解析选项钩子，用 pulldown-cmark 转换 Markdown，默认启用表格与删除线。
2. 执行正文 HTML 钩子，用 ammonia 清洗，只保留启用插件声明的扩展节点属性。
3. 从这份清洗结果提取指向本站 `/media/{uuid}` 的根相对图片地址，按 UUID 排序、去重。先按浏览器规则去掉首尾 ASCII 控制字符和空格、移除 TAB/LF/CR，并规范化反斜杠与普通或 `%2e` 编码的点路径；因此 `/other/../media/{uuid}` 与 `/media/{uuid}` 保护同一图片。查询串和 fragment 不改变身份；规范化后的 UUID 路径参数与 HTTP 入口一样只进行一次 UTF-8 百分号解码。规范化后仍含额外路径片段或重复斜杠的地址，以及非根相对地址和绝对 URL 不计入；`//host/...`、`/\\host/...` 等协议相对地址一律排除，不能因为规范化而误认外域图片。

注释、被清洗掉的标签、普通链接和纯文本不形成图片引用。独立封面由保存侧并入引用集合；它不属于正文渲染结果。媒体引用提取不会在异步数据库线程上再次解析 HTML。

[内容仓储](../crates/infrastructure/src/persistence/content.rs) 在事务外等待渲染，然后把源文、`content_html`、`content_render_version` 和媒体关系同事务提交。公开 Post/Page 详情直接读取保存的 HTML，不在每次访问时转换 Markdown。

`CONTENT_RENDER_VERSION` 标记 Markdown 转换、清洗与媒体引用提取的整个派生流水线；任一步规则改变时都需要递增版本（评论使用 `COMMENT_RENDER_VERSION`），并通过 CLI `blog rebuild-html` 或下述后台入口显式重建。当前正文版本 2 修复了带查询串、fragment、编码路径或浏览器路径规范化的图片引用。重建分批处理不匹配的记录，即使 HTML 字节相同也重新同步引用；以业务版本和源文作条件，避免覆盖并发编辑，不增加编辑版本或改变业务更新时间。启动本身不创建重建请求，结构迁移和普通业务命令不扫描历史 HTML；已明确提交的 queued 请求按计划执行，公开读取仍使用已存储的清洗结果。涉及清洗安全规则的升级必须在恢复公开访问前完成重建；媒体物理清理要求所有 Post/Page 派生版本已匹配。步骤与失败重试见[运维](operations-and-recovery.md#html-显式重建)。

显式重建也可由具有 `settings.manage` 的用户在后台「任务管理」立即启动或安排未来的一次性计划，固定每批 100 条、最多 100 批；后台复用当前正文/评论渲染运行时与同一逐记录 CAS、引用和审计事务，并在业务事务内核验当前任务租约。HTTP 只接受持久请求或读取进度，浏览器关闭后继续执行；queued 请求跨重启保留，运行中断后须明确重试并获得新 ID。恢复隔离时预检可读、启动及任务变更禁止，不改变业务编辑版本或更新时间。CLI 不进入任务队列；生命周期和跨进程边界见[后台重建操作](operations-and-recovery.md#在管理后台执行)与 [ADR-0020](adr/0020-persistent-admin-tasks.md)。

## 执行策略与实际预算

正文插件的启停和配置通过全站 `plugin_runtime.render_revision` 合入现有渲染版本，各插件状态与配置独立存入 `plugins`，具体编码、旧文章重建和资源钩子见[插件机制](plugins.md)。主题预检关闭运行期插件快照读取，保持固定数据和无数据库依赖；实际页面请求使用当前启用状态。

[`RenderingRuntime`](../crates/infrastructure/src/render_executor.rs) 为正文写入、评论与公开主题分配独立的许可池：正文和评论各默认 4 个，全部主题共用 16 个。评论预览和主题查询等待不会占用正文许可。三者仍共用 Tokio 阻塞线程池，许可隔离并非独立 CPU 或线程池。应用只调用异步端口，不持有 Tokio 信号量，也不自行使用 `spawn_blocking` 或超时。

主题主体先异步预取；同步 MiniJinja 渲染在阻塞池内运行。模板函数缺少请求缓存时，才从该阻塞线程通过 Tokio Handle 驱动带剩余截止时间的公开查询。桥接不会新建 runtime，也不在异步工作线程中 `block_on`；每次查询等待会占用当前渲染许可。

| 限制 | 当前默认值 | 作用范围 |
|---|---|---|
| 主题渲染并发 | 16 | 全部公开主题共享 |
| 正文渲染并发 | 4 | 正文转换与引用提取，独立于主题许可 |
| 评论渲染并发 | 4 | 评论提交与匿名预览，独立于文章保存和主题许可 |
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

缓存键包含完整源文和实际渲染版本（包括正文插件配置代数），值为 HTML、版本与正文引用；不缓存主题页面、数据库查询结果或媒体权限。单条结果超出容量时正常返回但不入缓存；命中不占渲染许可。tracing 记录命中、输入大小、排队和执行耗时、成功状态及媒体数量，不记录正文内容。请求内函数缓存随渲染结束释放。

## 转义与公开性

所有主题模板统一采用 MiniJinja 的 HTML 自动转义和严格未定义模式，不按文件扩展名切换转义策略；通过 `include`、继承或宏复用的 `.jinja`、`.j2` 及无扩展名辅助模板也使用 HTML 自动转义。清洗后的 `post.content_html` / `page.content_html` 可使用 `|safe`；普通标题、描述和用户资料保持自动转义。`url` 过滤器保留 `/` 并转义 HTML 属性特殊字符，供宿主生成的地址使用；它不替代 JavaScript、CSS 或任意 URL 的专门校验。

公开查询只允许已发布、公开、未删除的文章及已发布、公开的 Page。缺失内容可以返回 none 或 404，数据库失败、参数错误、预算耗尽和模板错误则保留为错误。当前没有公开页面缓存、跨请求函数缓存、全站 generation 或主题失败页面缓存；主题资源版本与内容公开性分别管理。

关于页面缓存、模板 API 扩展及外部集成的后续取舍，见[产品路线图](product-roadmap.md)与[扩展设计](extensions-and-data.md)。历史桥接、缓存方案及执行边界决策保留在 [ADR-0002](adr/0002-template-data-functions.md)、[ADR-0004](adr/0004-public-cache-generation.md)、[ADR-0015](adr/0015-rendered-content-runtime-and-module-boundaries.md)；其中规划方案不等于当前实现。

## 文章评论组件

内置 Default 和第三方 Paper 的文章模板通过 `data-comments-slug="{{ post.slug }}"` 挂载原生评论，容器初始带 `hidden`，同源 API 确认全站及文章评论均开启后才显示整个区域，加载 `/assets/comments.js` 与 `/assets/comments.css`。这些共享资源由 Rust 提供；昵称、错误和占位使用 DOM `textContent`，正文仅将服务端受限渲染的 `content_html` 放入 HTML 节点，不使用源文回退。公开列表和提交开关由同源 API 实时检查文章可见性。行为、分页及接口见[评论](comments.md)。

模板中的 `published_at`、`updated_at` 是按数据库站点设置 `site.time_zone` 格式化的展示文本，包含时区标记；`get_posts` / `get_post` 与页面主体使用该次渲染的同一时区快照；后台保存后，下次请求即生效。主题直接展示即可，不应按固定 UTC 格式解析这些文本。
