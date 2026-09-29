# 后台开发约定

本文承接 UI 与取数层 ADR 中需要持续维护的约定，覆盖内容、目录、媒体、身份、设置与评论。启动、联调和检查命令见[开发指南](development.md)，HTTP 载荷与错误码见[管理 API](admin-api.md)，选型理由见 [ADR-0011](adr/0011-admin-ui-library.md)、[ADR-0012](adr/0012-admin-data-layer.md) 和 [ADR-0013](adr/0013-tanstack-query.md)。

## 入口与组件

[App.tsx](../apps/admin/src/App.tsx)负责按路由懒加载屏幕；其内的 [AdminProviders](../apps/admin/src/providers.tsx)提供中文 locale、Ant Design 上下文、系统深浅色与每次挂载独立的 QueryClient。测试直接渲染根组件时也能获得相同环境。主题种子 token 集中维护；自定义组件使用 `theme.useToken()`，定制优先采用公开属性与语义插槽，不依赖组件内部 DOM。

[AdminLayout](../apps/admin/src/components/AdminLayout.tsx)管理导航、面包屑与账号操作，菜单当前不按权限隐藏。各屏可据权限禁用动作或停止无效查询，最终授权由后端执行；可见菜单不能被当作授权证明。

Post/Page 管理身份是稳定 UUID。生成编辑链接使用 [router.ts](../apps/admin/src/router.ts) 的 `paths.editPost(id)`、`paths.editPage(id)`，包括系列成员、媒体引用和评论关联跳转。slug 只表示公开地址，改名不能改变编辑会话身份。前端解析固定路径形状，UUID 合法性由 API 校验，见 [ADR-0014](adr/0014-content-commits-and-stable-admin-identity.md)。

需要持续阅读或处理的失败、冲突、部分成功提示使用内联 `Alert` 的 `title`。临时成功反馈并非全站禁用：评论审核与回复目前使用上下文 `message.success`。反馈停留方式应取决于用户是否仍需采取动作。

命令式确认使用 `App.useApp().modal.confirm`，以继承主题与 locale；声明式 `Modal`、`Popconfirm` 也有现行使用。危险操作应说明影响并标出危险按钮。按钮名称与加载反馈服务于操作本身，测试在对应弹窗内定位，不为了避免同名断言限制产品文案。

## HTTP 契约与客户端

[api/index.ts](../apps/admin/src/api/index.ts) 保留调用入口，端点按资源拆分；[client.ts](../apps/admin/src/api/client.ts) 统一认证凭据、错误码、请求编号和响应解析。JSON 成功响应须通过 [Zod 校验](../apps/admin/src/api/schemas.ts)，空响应命令必须返回 204；退出登录单独接受服务端跳转后的 HTML。协议错误使用 `ApiProtocolError`，保留实际 HTTP 状态与请求编号，不自动重试写入，也不显示响应中的原始敏感内容。新增响应字段允许兼容，缺失必填字段、类型错误、未知封闭枚举及不安全整数会被拒绝。

[generated.ts](../apps/admin/src/api/generated.ts) 由实际 HTTP DTO 的 ts-rs 派生生成。依赖仅在 interfaces；直接返回应用视图的端点先转换为接口层 DTO。包装转换穷尽解构应用字段，避免内部字段变化静默丢失。前端输入类型引用生成结果，响应类型从校验器推导，校验器逐字段受生成类型约束；新增、删除或改型字段必须同步校验器。筛选状态等纯前端类型仍由前端维护。

```sh
cargo run -p interfaces --example export_admin_contract
cargo run -p interfaces --example export_admin_contract -- --check
pnpm --dir apps/admin typecheck
```

生成文件纳入版本控制，CI 与 `scripts/check.sh` 检查是否过期。UUID 对应字符串，JSON 整数对应 number；运行时检查安全整数范围。自定义 PATCH 反序列化使用 `serde(default, with = "double_option")`，配合 `ts(as = "Option<T>", optional = nullable)` 显式声明可选/可空类型；这种写法由 ts-rs 支持，保留其他不兼容 serde 属性的警告。Rust 和前端测试验证缺省、不为空的值与 null 清空三态。生成类型不替代实际响应校验，也不生成 domain 或基础设施模型。

## 表单与编辑会话

[文章编辑器](../apps/admin/src/screens/PostEditScreen.tsx)通过 [usePostEditor](../apps/admin/src/screens/postEditor/usePostEditor.ts) 管理请求、服务器基线和恢复副本；[form.ts](../apps/admin/src/screens/postEditor/form.ts) 负责表单归一化与逐字段合并，[PostMetadataFields](../apps/admin/src/screens/postEditor/PostMetadataFields.tsx) 负责目录查询和选择控件。文章输入仅存于 Ant Design Form store，`Form.useWatch` 触发界面更新，不再手工双写整个表单镜像。订阅通知会批量延迟，因此异步响应和离开确认必须同步读取 store；请求完成后的渲染也读取当前值，避免用旧订阅值写入本机副本。

[页面编辑器](../apps/admin/src/screens/PageEditScreen.tsx)和[设置屏](../apps/admin/src/screens/SettingsScreen.tsx)目前仍由 `onValuesChange` 和统一写入函数维护渲染镜像。程序调用 `setFieldsValue` 不会触发用户输入回调。

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

正文预览调用[非持久化预览接口](admin-api.md#文章与回收站)，使用服务端清洗后的 HTML；预览请求不更新编辑基线或本机副本，不声称覆盖完整主题。冲突对比、预览和本地存储提示均须按当前账号/UUID 隔离，旧请求不得回填新编辑目标。

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
