# 后台开发约定

本文承接 UI 与取数层 ADR 中需要持续维护的约定，覆盖内容、目录、媒体、身份、设置与评论。启动、联调和检查命令见[开发指南](development.md)，HTTP 载荷与错误码见[管理 API](admin-api.md)，选型理由见 [ADR-0011](adr/0011-admin-ui-library.md)、[ADR-0012](adr/0012-admin-data-layer.md) 和 [ADR-0013](adr/0013-tanstack-query.md)。

## 入口与组件

[App.tsx](../apps/admin/src/App.tsx)负责按路由懒加载屏幕；其内的 [AdminProviders](../apps/admin/src/providers.tsx)提供中文 locale、Ant Design 上下文、系统深浅色与每次挂载独立的 QueryClient。测试直接渲染根组件时也能获得相同环境。主题种子 token 集中维护；自定义组件使用 `theme.useToken()`，定制优先采用公开属性与语义插槽，不依赖组件内部 DOM。

[AdminLayout](../apps/admin/src/components/AdminLayout.tsx)管理导航、面包屑与账号操作，改密与头像弹窗在打开时懒加载，菜单当前不按权限隐藏。各屏可据权限禁用动作或停止无效查询，最终授权由后端执行；可见菜单不能被当作授权证明。

Post/Page 管理身份是稳定 UUID。生成编辑链接使用 [router.ts](../apps/admin/src/router.ts) 的 `paths.editPost(id)`、`paths.editPage(id)`，包括系列成员、媒体引用和评论关联跳转。slug 只表示公开地址，改名不能改变编辑会话身份。前端解析固定路径形状，UUID 合法性由 API 校验，见 [ADR-0014](adr/0014-content-commits-and-stable-admin-identity.md)。

需要持续阅读或处理的失败、冲突、部分成功提示使用内联 `Alert` 的 `title`。临时成功反馈并非全站禁用：评论审核与回复目前使用上下文 `message.success`。反馈停留方式应取决于用户是否仍需采取动作。

命令式确认使用 `App.useApp().modal.confirm`，以继承主题与 locale；声明式 `Modal`、`Popconfirm` 也有现行使用。危险操作应说明影响并标出危险按钮。按钮名称与加载反馈服务于操作本身，测试在对应弹窗内定位，不为了避免同名断言限制产品文案。

## HTTP 契约与客户端

[api/index.ts](../apps/admin/src/api/index.ts) 保留兼容入口；应用运行时直接导入 `api/identity`、`api/posts` 等资源适配器及各自的 `api/schemas/` 校验器，避免认证入口加载全部领域。测试同样 mock 实际适配器，不通过兼容聚合对象替换方法。[client.ts](../apps/admin/src/api/client.ts) 统一认证凭据、错误码、请求编号和响应解析。JSON 成功响应须通过 Zod 校验，空响应命令必须返回 204；退出登录单独接受服务端跳转后的 HTML。协议错误使用 `ApiProtocolError`，保留实际 HTTP 状态与请求编号，不自动重试写入，也不显示响应中的原始敏感内容。新增响应字段允许兼容，缺失必填字段、类型错误、未知封闭枚举及不安全整数会被拒绝。

认证使用轻量 [timeZoneContext.tsx](../apps/admin/src/timeZoneContext.tsx)；时间显示与拒绝夏令时重复/不存在时段的转换留在 [timeZone.tsx](../apps/admin/src/timeZone.tsx)。构建为 Temporal/jsbi 设置独立分组，避免通用 runtime helper 将排期依赖重新带入首屏。运行 `node apps/admin/scripts/bundle-size.mjs apps/admin/dist` 可统计入口全部静态 JavaScript 依赖和 gzip 总量；评价拆分收益时比较总量，不能只看最大 chunk。

[generated.ts](../apps/admin/src/api/generated.ts) 由实际 HTTP DTO 的 ts-rs 派生生成。依赖仅在 interfaces；直接返回应用视图的端点先转换为接口层 DTO。包装转换穷尽解构应用字段，避免内部字段变化静默丢失。前端输入类型引用生成结果，响应类型从校验器推导，校验器逐字段受生成类型约束；新增、删除或改型字段必须同步校验器。筛选状态等纯前端类型仍由前端维护。

```sh
cargo run -p interfaces --example export_admin_contract
cargo run -p interfaces --example export_admin_contract -- --check
pnpm --dir apps/admin typecheck
```

生成文件纳入版本控制，CI 与 `scripts/check.sh` 检查是否过期。UUID 对应字符串，JSON 整数对应 number；运行时检查安全整数范围。自定义 PATCH 反序列化使用 `serde(default, with = "double_option")`，配合 `ts(as = "Option<T>", optional = nullable)` 显式声明可选/可空类型；这种写法由 ts-rs 支持，保留其他不兼容 serde 属性的警告。Rust 和前端测试验证缺省、不为空的值与 null 清空三态。生成类型不替代实际响应校验，也不生成 domain 或基础设施模型。

## 表单与编辑会话

[文章编辑器](../apps/admin/src/screens/PostEditScreen.tsx)通过 [usePostEditor](../apps/admin/src/screens/postEditor/usePostEditor.ts) 管理请求、服务器基线和恢复副本；[form.ts](../apps/admin/src/screens/postEditor/form.ts) 负责表单归一化与逐字段合并，[PostMetadataFields](../apps/admin/src/screens/postEditor/PostMetadataFields.tsx) 负责目录查询和选择控件。文章输入仅存于 Ant Design Form store，`Form.useWatch` 触发界面更新，不再手工双写整个表单镜像。订阅通知会批量延迟，因此异步响应和离开确认必须同步读取 store；请求完成后的渲染也读取当前值，避免用旧订阅值写入本机副本。

[页面编辑器](../apps/admin/src/screens/PageEditScreen.tsx)只负责视图，[usePageEditor](../apps/admin/src/screens/pageEditor/usePageEditor.ts)管理请求与服务器基线，[form.ts](../apps/admin/src/screens/pageEditor/form.ts)负责纯表单转换及合并。[设置屏](../apps/admin/src/screens/SettingsScreen.tsx)组合独立的常规、主题、账号和保留期表单；常规与主题实现位于 `screens/settings/`，各组拥有自己的基线，向父级回传 dirty，由父级统一登记离开保护。未保存的主题选择同样受保护；主题卡片支持键盘 Enter/Space 和选择状态。Page/设置仍由 `onValuesChange` 和统一写入函数维护渲染镜像，程序调用 `setFieldsValue` 不会触发用户输入回调。

- 用当前值与最近服务器基线比较 dirty，不能以 touched 状态代替。程序回填与用户实际修改不是同一件事。
- 发请求时保存提交快照。响应只覆盖等待期间没有继续修改的字段；保留新增输入，并明确提示其尚未保存。标签按集合比较，系列及序号作为关联字段一起处理。
- 编辑器记录已加载实体 ID；切换到另一 ID 尚未成功加载时禁止保存、发布等动作，不能把上一篇内容和版本提交到新地址。
- 创建成功先应用服务器结果和等待期间输入，再进入新 ID 地址，避免路由变化触发重载吞掉输入。
- 发布或预约前如有未保存内容，先保存，再使用返回版本切换状态。撤回、归档只改变服务器状态并保留本地输入，不能先保存未完成修改。状态响应只更新相应状态，不整体回填旧正文。
- 版本冲突保留本地输入，并读取服务器快照展示字段对比。重新加载会丢弃本地值；确认覆盖提交对比中展示的版本，不能在确认后重新读取版本并静默覆盖较新的修改。其他 409 如 slug 占用不能靠版本覆盖解决。

正文保持 Markdown，HTML 由服务端派生。需要对齐后端“字符”上限的字段使用 [codePointLength](../apps/admin/src/text.ts)，不能直接把 HTML `maxLength` 的 UTF-16 长度当作码点数。当前评论回复框仍使用 `maxLength`，不应据此声称所有输入已统一计数。

[未保存保护](../apps/admin/src/unsaved.tsx)由屏幕登记 dirty 或即时读取函数，外壳和屏内主动离开入口调用 `useLeaveConfirmation`；刷新、关闭标签页或跨文档离开使用 `beforeunload`。后台历史前进、后退由 [NavigationHistory](../apps/admin/src/navigationHistory.ts) 统一确认：历史项记录位置，先恢复原位置，确认前不切换渲染路由；取消保留原编辑组件和前进栈，确认后再前往目标位置。支持一次跨多个历史项返回，重复点击不叠加确认框。

保存、删除成功后的程序跳转不需要再次确认；创建响应在确认期间返回时先结束待决导航，再更新内容地址，保留保存期间的新输入。

离开保护覆盖已登记的文章、页面、个人资料和设置等表单，不代表每个短表单都已登记。Post/Page 另通过 [localDraft.tsx](../apps/admin/src/localDraft.tsx) 和 [draftStorage.ts](../apps/admin/src/draftStorage.ts) 保存本机恢复副本：v2 按账号、类型、UUID/新建、标签和页面实例隔离；v1 仅作为恢复候选兼容。恢复复制候选到当前实例，保留原版本且不联网保存，不删除源实例数据。成功提交/手动删除仅清理当前实例拥有的槽，等待期间的新输入继续保留；忽略候选只在当前标签记录已处理修订。副本不自动过期或随退出清除，存储失败独立于服务器保存结果显示。完整保留与隐私边界见[当前正文与资源身份](content-lifecycle.md#1-当前正文与资源身份)。

草稿输入按固定 400ms 窗口合并写盘，持续输入不会无限推迟保存，render 不再序列化完整正文。`pagehide`、`beforeunload`、页面隐藏与卸载时补写最新 Form store；身份/实体切换先处理旧槽，成功提交、手动删除和创建后的 ID 迁移取消待写任务，不能复活旧槽。保存时间只提供反馈，不触发新一次写盘；恢复克隆仍立即持久化。

编辑器的“管理本机副本”按需加载 [LocalDraftManager](../apps/admin/src/components/LocalDraftManager.tsx)，仅枚举当前账号命名空间，显示标题、保存时间、服务器基线版本、预计占用及纯文本预览。恢复到当前实体前按当前表单模板严格校验并复核快照；其它实体通过离开确认打开对应编辑器恢复。安全删除是额外能力：[draftActivity.ts](../apps/admin/src/draftActivity.ts) 在编辑生命周期持有每个自有槽的 Web Lock，取得锁后才能声明 `coordination: web-lock-v1`；删除确认后必须取得同名空闲锁，并比较原始快照，拒绝活跃或后来更新的源副本。旧 v1/v2 未声明协议的副本及无锁 API 的环境都保留查看/恢复，管理器不推断它们已闲置；普通输入保存不依赖锁。离页/卸载释放锁，BFCache 返回重新取得并立即补写最新表单，不能因为暂停前写过就跳过已被清理的槽；列表通过已有 storage 事件或手动刷新更新，副本仍不会自动过期。

Post/Page 共用 [MarkdownEditor](../apps/admin/src/components/MarkdownEditor.tsx)，使用锁定版本的 Vditor 4，提供即时渲染（IR）、双栏（SV）和源码模式；桌面默认双栏，窄屏提供即时渲染/源码切换。编辑时在浏览器内渲染，双栏按滚动进度双向联动，公式、图表或图片改变预览高度后重新对齐。源文仍由原有 Form store 管理；关闭 Vditor 独立缓存，继续使用按账号/目标隔离的本机草稿。模式切换不会单独改写源文或造成脏状态，源码模式保留末尾换行原样；IR 的真实编辑会经过 Lute 规范化。选区插图通过编辑模式适配器执行，IR 的 DOM 选区在打开媒体面板后仍可恢复。保存期间的新输入、新稿取得 UUID 的状态迁移语义保持不变。

Vditor 运行时资源由 [vditorAssets.ts](../apps/admin/build/vditorAssets.ts) 自动打包到带内容摘要的 `/admin/assets/vditor-*/`，开发和生产均为同源，不使用外部 CDN。编辑器本体按需加载，数学和图表使用与前台插件相同的锁定 KaTeX/Mermaid 依赖；加载失败时保留源码输入和重试入口。编辑器内的数学/图表用于写作辅助，不取决于前台插件启停。当前接入只启用数学和 Mermaid 扩展，其它图表围栏保留为代码。公式语法错误时保留源文，并显示“公式渲染失败，请检查语法。”；提示不写入 Markdown，复制公式仍只复制源文。Vditor 的 [pnpm 补丁](../apps/admin/patches/vditor@4.0.0.patch) 限定渲染器范围、强制 Mermaid strict 和纯文本错误信息、修正初始化销毁检查，并把图标脚本改为满足站点 CSP 的同源加载；升级依赖须重验补丁及真实浏览器用例。

编辑时使用 Vditor 即时渲染或双栏预览；“发布效果预览”由用户主动触发，调用[非持久化预览接口](admin-api.md#文章与回收站)，在沙箱文档展示服务端正文 HTML 与插件资源。正文变化时旧结果失效，最终完整页面仍取决于当前主题。冲突对比和本地存储提示均须按当前账号/UUID 隔离，旧请求不得回填新编辑目标。

Post/Page 的“保存修改草稿”将已发布内容保存到服务器，公开页面保持上一发布结果；“发布更新”先保存当前输入，再按返回版本发布。`RevisionHistory` 确认后恢复到编辑稿，采用编辑器现有合并规则保留请求期间的新输入。历史恢复和发布均使用 CAS，冲突时保留本地编辑并展示服务器对比。

Vditor 工具栏只保留一个“插入图片”入口，打开当前视口内的弹窗，同时提供上传新图片和从媒体库选择；媒体库支持搜索、分页和替代文字。入口在拥有上传或读取权限时显示，弹窗内按各自权限提供操作，只读状态禁用入口。上传期间禁用重复文件选择，成功插入后关闭弹窗，失败保留弹窗和错误信息。正文拖入、粘贴和弹窗上传共用文件校验、上传反馈及选区插入流程。

正文与封面图片上传同样属于发起时的编辑会话：切换到另一篇内容后，已完成的上传仍刷新媒体库，旧回调不再改动正文、封面或显示反馈；新稿保存后获得 UUID 仍属于同一次编辑。

自身槽的待恢复候选未处理时禁用编辑；其他实例候选不能阻断当前窗口持续保存新输入。恢复先持久化当前实例的克隆，再记录源修订已处理，克隆失败不得清除或持久隐藏源修订。保存飞行期间禁用恢复，防止旧副本与返回的新基线交错。

## 查询与缓存

[默认策略](../apps/admin/src/queryClient.ts)是 30 秒新鲜期、关闭窗口聚焦重取，只对 `ApiError.status >= 500` 最多重试两次；mutation 不自动重试。普通网络异常不满足这条 `ApiError` 判断。401 由 [API 客户端](../apps/admin/src/api/client.ts) 与[认证层](../apps/admin/src/auth.tsx)处理，Query 只负责不重试，尚未统一为 Query 级认证处理器。

查询缓存属于当前认证会话：转入登录页或切换账号会重新创建 QueryClient，并清除旧查询与编辑器状态。CSRF token 变化时取消上一会话的在途请求，旧请求的 401 不得退出新会话。本机恢复副本仍按既有账号隔离规则保留。

媒体库、封面选择器和正文图片面板共用 [mediaPageQuery](../apps/admin/src/mediaQueries.ts) 的查询键、取消信号和缓存。封面弹窗关闭或没有读取权限时不加载；重新打开时重取，并可先显示缓存。上传后的失效仍通过统一写入影响表执行。

列表、目录、设置、媒体与系列成员使用 Query。系列成员按稳定 ID 和目录版本独立缓存，旧版本的慢响应不会覆盖新版本成员；目录或成员刷新期间禁用重排，避免把新版本和旧顺序一起提交。Post/Page 详情仍直接请求并维护编辑基线。设置首次读取可初始化表单，后续后台重取不能无条件重新填表；版本冲突取服务器当前值时使用直接请求。

共享查询键集中在 `queryKeys`，包括评论列表的页码、状态与文章过滤，以及全站/单篇评论开关。新增或修改查询时要把实际影响结果的页码、过滤条件、资源身份放入键，并明确共享范围。

关联写入统一通过 [invalidateAfterWrite](../apps/admin/src/queryEffects.ts) 声明影响范围。每一步提交成功后立即失效，包括先保存后发布、逐张上传等流程，不能等整个动作成功才刷新。该入口同时使审计查询失效；表单基线仍由提交响应更新，不用后台重取覆盖输入。

| 写入路径 | 当前更新范围 |
|---|---|
| Post 创建、保存、覆盖、发布、预约、撤回、归档、移入回收站、恢复、永久删除 | 全部文章列表页及筛选、全部回收站页、标签/分类/系列统计、媒体列表及使用位置、评论关联信息 |
| Page 内容/状态写入、移入回收站、恢复、永久删除 | 全部页面列表页及筛选、全部页面回收站页、媒体列表及使用位置 |
| 媒体库上传、软删除、恢复；封面选择器直接上传；正文粘贴/拖入上传 | `mediaAll()` 前缀，包含正常库、回收站全部分页与使用位置；批次中已成功的上传不受后续失败影响 |
| 标签、分类、系列写入 | 各自目录键、文章列表与回收站；系列族同时覆盖成员查询，并刷新媒体引用 |
| 用户创建、角色分配或移除 | `users()` |
| 个人资料或头像保存 | 用户列表、媒体列表及使用位置；另刷新当前身份资料 |
| 站点设置保存 | 站点设置键、媒体列表及使用位置 |
| 主题设置保存 | 主题设置键；选择冲突快照重载时写入对应缓存 |
| 评论审核、删除、回复 | `commentsAll()` 前缀；审核或删除失败后也会重取 |
| 全站 / 单篇评论开关 | 成功响应写入自身 policy 键，刷新评论列表；单篇同时刷新 Post 关联视图；失败使自身键失效 |

评论行为见 [CommentListScreen](../apps/admin/src/screens/CommentListScreen.tsx) 和 [CommentSwitch](../apps/admin/src/components/CommentSwitch.tsx)。后者使用 `useMutation`；复杂内容写入仍以显式函数编排，两种方式都必须保留版本和错误语义。审核使最后一页清空时，列表在重取完成后回到最后有效页。

新增关联写入时应检查所有读取方，并更新影响表及返回路径测试。这里保证当前后台实例中由这些入口提交后的刷新；其他浏览器、CLI、预约任务或维护任务的变更仍需重新加载，不属于跨标签页实时同步。

普通文章/页面列表通过 `useContentList` 管理 `page/status/visibility`，查询键为 `postList(filter)` / `pageList(filter)`；`posts()` / `pages()` 是用于写后失效的整族前缀。筛选变化回到首页，翻页时保留上一份数据与对应页码，筛选变化不沿用旧条件的行；删除后超过末页时直接回到最后有效页。

分页增删可能移动其他页，应按查询族失效。非活跃查询通常先标记过期，重新挂载时再取数。回收站使用 `keepPreviousData`，展示页码和行取同一份服务器数据，不把请求中的页码与上一页内容混用。`isPending`、`isFetching` 与成功空列表含义不同，禁用查询时也应先处理权限分支。

## 错误与写入结果

[apiError.ts](../apps/admin/src/apiError.ts)提供通用文案和管理权限文案，均可保留请求编号。登录、重新认证及具体业务错误应采用对应语义，不一律加权限前缀。接口字段、三态更新与错误码以[管理 API](admin-api.md)为准，不在 UI 层另定义一套协议。

取数错误与动作错误分开管理，避免后台重取覆盖刚刚发生的操作失败。写入成功而后续刷新失败时，要让用户知道写入已经生效，不能显示成从未保存；多步骤动作同样分别处理成功部分。自动重试不能代替用户处理版本冲突。

## 测试约定

测试以最终界面、有效载荷和是否允许写入为主要证据。测试与构建命令统一见[开发指南](development.md#检查与测试)。

`pnpm typecheck` 检查 `src/`、`tests/`，并通过单独配置检查 `e2e/` 和 Playwright 配置。测试 fixture 必须符合当前 API 类型；可复用 `httpFixtures.ts` 的身份与文章响应、`contentFixtures.ts` 的分页与列表摘要构造函数。

- 异步挂载、表单校验、查询通知和弹窗收尾使用 `findBy*` 或 `waitFor` 等待可观察结果，不固定实现内部的调度次数。
- 验证请求期间输入时，先确认请求已发出，再改变输入并完成受控响应，断言新输入和未保存提示都保留。
- 验证写后缓存更新时，先访问列表形成旧缓存，再编辑并返回列表，断言新值；直接让初始查询返回新值无法证明失效有效。
- 验证历史离开保护时，调用真实 `history.back/forward/go`，断言取消后仍是同一个输入节点、正文未重取、前进栈未被截断；同时覆盖确认后离开和创建请求在确认期间完成。
- 请求次数只用于确有约束的行为，例如取消后不写、重复点击不重复提交、身份加载失败不写入。证明列表刷新应断言列表结果。
- 错误测试与重试测试分开控制：持续失败需覆盖所有尝试；不要让仅失败一次的 mock 被后续成功重试掩盖，也不要只为缩短等待把服务端故障改成语义不同的权限错误。
- 在当前对话框或控件范围内按角色、名称定位。Select、Modal 等按实际交互方式操作，不依赖内部 class、隐藏残留节点或动画结构。

可复用样例在 [editor.test.tsx](../apps/admin/tests/editor.test.tsx)、[listScreens.test.tsx](../apps/admin/tests/listScreens.test.tsx)、[settings.test.tsx](../apps/admin/tests/settings.test.tsx) 和 [CommentListScreen.test.tsx](../apps/admin/src/screens/CommentListScreen.test.tsx)。[testSetup.ts](../apps/admin/src/testSetup.ts)只补最小浏览器 API，不提供真实布局；组件测试不能替代构建后的浏览器交互与视觉核验。

生产构建的浏览器冒烟测试放在 `e2e/`，通过[验收脚本的 `--browser` 选项](acceptance.md)在临时站点运行。只覆盖跨层关键流程，不重复枚举已由单元测试覆盖的字段和错误分支。
