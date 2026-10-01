# 插件机制

插件是独立的站点功能，管理入口为 `/admin/plugins`。框架提供注册、启停、配置、正文钩子和前台资源钩子；目前内置「Markdown 增强」插件，支持 KaTeX 公式与 Mermaid 图表。插件默认关闭，原有 Markdown 行为保持不变。

## Markdown 增强

插件 ID 为 `markdown-enhance`，包含 `math`（公式）和 `mermaid`（图表）两个布尔配置，默认均为 `true`；只有启用插件后才生效。

````markdown
行内公式：$E=mc^2$。普通美元符号可以写成 \$。

$$
\int_0^1 x^2\,dx=\frac{1}{3}
$$

```mermaid
graph LR
    A[开始] --> B[完成]
```
````

公式使用 pulldown-cmark 的 math 解析选项，保存为含转义 TeX 的 `span.math.math-inline` / `span.math.math-display`；图表保存为 `pre > code.language-mermaid`。代码示例和转义的美元符号遵循 Markdown 语法，不扫描整篇 HTML 猜测公式。浏览器在 `data-content-root` 内完成公式排版和 SVG 生成，不把 KaTeX DOM 或 SVG 写回数据库。公式沿用 KaTeX 支持的 TeX 子集。

文章、独立页面和后台预览使用相同的本地资源。公式开启时加载 KaTeX CSS、字体及 `math.js`；图表开启时加载 `mermaid.js`。启用图表时所有文章和独立页面都会下载图表脚本，浏览器可缓存；没有图表节点时不执行图表布局。脚本使用 `defer`，浏览器完成渲染前节点仍显示源文。当前完整 Mermaid 包含 ELK 等布局能力，脚本约 5.3 MB（gzip 约 1.5 MB）；公式脚本约 269 KB（gzip 约 78 KB）。实际传输大小取决于 HTTP 压缩配置。

单个扩展语法错误保留源文并显示提示，其余节点继续渲染。KaTeX 禁止不可信 HTML 命令；Mermaid 使用 strict 模式，不启用图表点击回调。长公式和图表容器允许横向滚动，图表使用浅色画布以保持两个主题中的可读性。

后台“正文预览”使用仅允许脚本的沙箱 iframe，复用同一份插件资源，并限制资源来源。预览不访问后台 DOM、存储或 API；公开的插件字体允许跨来源读取，以支持沙箱的不透明来源。更改输入或收起预览会销毁旧预览，重新预览时取最新插件状态。

首次开启、切换功能或停用后，通过“任务管理 → 内容重建”更新历史文章和独立页面；新保存的正文立即应用当前规则。安装这个默认关闭的新插件不会单独改变原有渲染版本，首次启用通过 `render_revision` 标记历史 HTML 过期。

浏览器源代码在 `crates/infrastructure/src/plugins/markdown_enhance/browser/`，已构建资源在 `crates/infrastructure/assets/markdown-enhance/`，随 Rust 二进制发布。依赖固定于 `apps/admin/package.json` 和锁文件，资源中保留第三方许可证。更新后执行：

```sh
pnpm --dir apps/admin install --frozen-lockfile
pnpm --dir apps/admin build:plugins
pnpm --dir apps/admin check:plugins
pnpm --dir apps/admin exec playwright test e2e/markdown-enhance.spec.ts
```

构建将依赖打成独立经典脚本，不产生运行时模块下载；CI 检查生成资源与源码、锁定依赖是否一致。普通 Rust 构建不需要 Node 或联网下载这些库。未来更改已发布插件的正文输出规则时，仍需按下文递增核心渲染版本。

首版插件是随 Rust 程序构建发布的可信代码，不提供上传、下载、动态库或隔离沙箱。插件可只实现页面资源钩子，也可同时实现正文钩子，分类不绑定 Markdown。后续新增能力应增加有明确输入输出的钩子，不开放任意 SQL 或核心事务修改入口。

## 注册与配置

注册入口是 [`PluginCatalog::builtins`](../crates/infrastructure/src/plugins.rs)，每个 `PluginRegistration` 包含：

| 字段 | 用途 |
|---|---|
| `definition` | 稳定 ID、名称、说明、版本、配置字段；`hooks` 由实际注册的处理器生成 |
| `content` | 可选 `ContentHook`，处理正文 |
| `html_rules` | 正文节点需要保留的 class 和 `data-*` 属性 |
| `page_head` | 可选 `PageHeadHook`，声明当前页面需要的资源 |
| `files` | 相对路径到静态字节的快照，通常使用 `include_bytes!` 打包 |

ID 长度 1–48，首字符为小写字母，其他字符只允许小写字母、数字、`-`。重复 ID、非法路径和节点规则在装配时拒绝。钩子按插件 ID 字典序执行；单插件返回的资源保持声明顺序。首版没有插件依赖解析或动态加载器。

配置字段支持布尔、32 位整数和文本，默认值同时声明类型。最多 16 个字段，文本最多 2,048 UTF-8 字节。未声明的字段和类型不符的值拒绝保存，缺省字段使用默认值；配置字段均为非秘密值。清单及其配置契约的兼容升级由插件实现负责。

状态只保存在已有 `settings` 表的 `key = 'plugins'` 行。例如：

```json
{
  "schema_version": 1,
  "render_revision": 1,
  "plugins": {
    "example-extension": {
      "enabled": true,
      "config": { "compact": false }
    }
  }
}
```

没有该行时所有插件默认关闭。插件程序移除后保留配置，后台显示“插件不可用”，允许停用，不能启用。这个 JSON 是全站设置，不是文章扩展清单；Post/Page 不增加字段。

管理读写均要求独立的 `plugins.manage` 权限，默认授予 Admin，`settings.manage` 不包含它。接口为：

- `GET /api/admin/v1/plugins`：注册清单、启停状态、生效配置和全局 `version`。
- `PUT /api/admin/v1/plugins/{id}`：`{ enabled, config, expected_version }`，返回更新后的完整列表。

首次保存使用 `expected_version = 0`，后续使用读取或写响应中的版本。版本过期返回 `409 version_conflict`，包括重复提交；相同状态且版本匹配不写入、不递增版本、不追加审计。状态与 `plugin.configure` 审计同事务提交，审计不保存配置值。沿用会话、CSRF、Origin 和 `no-store` 契约，恢复隔离环境禁止修改。

## 正文钩子

每次操作读取一份已启用插件快照，配置在本次渲染中固定：

```text
Markdown → ContentHook.prepare → Markdown 解析
         → ContentHook.transform_html → 最终 HTML 清洗
         → 提取媒体引用 → 保存 content_html 和 content_render_version
```

`prepare` 可以处理源文并设置解析选项；`transform_html` 可以转换解析后的 HTML。内置 Markdown 增强通过 math 选项保留转义后的公式源文节点，前台脚本再完成排版。钩子在有界阻塞执行器中运行，沿用源文/HTML 大小、并发和超时限制。失败返回渲染错误，不保存半成品；不静默降级成另一套 HTML。

最终清洗不可绕过。`html_rules` 目前只允许为 `span/div/pre/code` 声明具体 class 和 `data-*` 属性；脚本、事件处理属性、行内 style 和危险 URL 仍被清理。配置不参与扩大允许列表。不要用全局允许 `class`、`style` 或任意属性来支持扩展。

文章、独立页面、后台 Markdown 预览、CLI 保存和 HTML 重建共用这条正文流水线；评论继续使用原有受限渲染器。正文预览接口返回 `content_html` 和宿主资源钩子生成的 `head_html`，两者共用一次插件快照；评论预览仍仅返回 `content_html`。

## 前台资源钩子

`PageHeadHook::assets(page, config)` 接收页面种类（首页、文章、独立页面、标签、分类、系列、正文预览），返回当前插件快照内的 CSS/JS 文件。无须扫描文章或保存每篇文章的功能清单。Markdown 增强在文章、独立页面和正文预览输出资源；其他类型插件自行决定适用页面。

主题在 `<head>` 内调用一次：

```html
{{ plugin_head() }}
```

default/paper 已接入，正文容器统一提供 `data-content-root`，插件脚本可在该容器中查找自身节点。第三方主题需添加相同调用，并可在 `theme.json.required_functions` 中声明 `plugin_head`。

输出由宿主生成并去重：

```html
<link rel="stylesheet" href="/assets/plugins/example-extension/资源哈希/display.css">
<script src="/assets/plugins/example-extension/资源哈希/display.js" defer></script>
```

JS 使用经典外部脚本的 `defer`，同页按声明顺序执行；CSS 使用普通样式表链接。没有行内脚本、资源 URL 配置或客户端动态加载器。`files` 也可包含 CSS 引用的字体/图片，使用相对路径。全部文件属于公开资源，不能放入秘密。

资源版本是路径和字节内容的完整 SHA-256。HTTP 只返回注册快照中的文件，正确设置 MIME、`nosniff` 和一年 immutable 缓存；缺失文件/版本返回 404。停用后新页面不再输出资源，已缓存的文件可以继续读取。插件更新后跨版本资源保留与主题相同，由部署层负责。

页面渲染会读取当前数据库状态，不依赖进程内启停缓存，重启和多个服务进程均使用相同配置。钩子资源在主题渲染或正文预览时生成；后台仅在隔离预览中执行，不载入 SPA 主界面、RSS 或 sitemap。浏览器仍需完成脚本加载和节点渲染才能看到扩展效果，`defer` 不会将图表变成服务端已绘制结果。

## 缓存和历史 HTML

继续使用原有 `content_render_version`，计算方式为：

```text
content_render_version = render_revision × 1024 + CONTENT_RENDER_VERSION
```

`render_revision` 是全站正文插件配置代数：启停正文插件，或修改已启用正文插件配置时递增；仅前台资源插件或关闭状态下的配置修改不递增。它不是文章功能清单。核心版本保留低 10 位，必须为 1–1023；配置代数上限为 2,097,151，达到上限拒绝继续递增，禁止回绕。

**修改核心渲染规则或发布改变正文输出的插件代码时，必须递增 `application::ports::CONTENT_RENDER_VERSION`。** 仅更新前台静态资源不需要改变正文版本。现有版本 2 对应配置代数 0，因此空插件目录不会要求全站重建。

Markdown 缓存键包含实际渲染版本。持久化总是使用生成该 HTML 的快照版本，即使渲染期间插件配置改变也不会错误标成新版。原有 HTML 重建预检据此识别过期文章和页面，重建时保持正文编辑版本不变，并同步媒体引用。媒体清理也核对当前流水线版本，旧引用尚未重建时拒绝物理清理。

启停不立即重写全部历史文章。前台资源在下一次页面请求生效；新保存的正文立即使用新规则；旧正文通过后台“任务管理 → 内容重建”或 `blog rebuild-html` 更新。配置切换前已开始的渲染可能仍提交旧版本，下次预检会继续列出它；重建任务结束后以待处理计数为准。停用到重建完成之间，旧扩展节点可能显示其源文。

测试插件只存在于 [`infrastructure/tests/plugins.rs`](../crates/infrastructure/tests/plugins.rs)，覆盖启停、配置、缓存、清洗、页面作用域、资源去重、持久化和重建；没有把测试插件注册为实际功能。内置 Markdown 增强另有 Rust 流水线测试与 Playwright 浏览器测试，覆盖两个主题、后台沙箱预览、字体和错误回退。
