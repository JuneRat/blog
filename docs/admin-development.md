# 后台开发约定

本文承接 UI 与取数层 ADR 中需要持续维护的约定，覆盖内容、目录、媒体、身份、设置与评论。启动、联调和检查命令见[开发指南](development.md)，HTTP 载荷与错误码见[管理 API](admin-api.md)，选型理由见 [ADR-0011](adr/0011-admin-ui-library.md)、[ADR-0012](adr/0012-admin-data-layer.md) 和 [ADR-0013](adr/0013-tanstack-query.md)。

## 入口与组件

[App.tsx](../apps/admin/src/App.tsx)负责按路由懒加载屏幕；其内的 [AdminProviders](../apps/admin/src/providers.tsx)提供中文 locale、Ant Design 上下文、系统深浅色与每次挂载独立的 QueryClient。测试直接渲染根组件时也能获得相同环境。主题种子 token 集中维护；自定义组件使用 `theme.useToken()`，定制优先采用公开属性与语义插槽，不依赖组件内部 DOM。

[AdminLayout](../apps/admin/src/components/AdminLayout.tsx)管理导航、面包屑与账号操作，菜单当前不按权限隐藏。各屏可据权限禁用动作或停止无效查询，最终授权由后端执行；可见菜单不能被当作授权证明。

Post/Page 管理身份是稳定 UUID。生成编辑链接使用 [router.ts](../apps/admin/src/router.ts) 的 `paths.editPost(id)`、`paths.editPage(id)`，包括系列成员、媒体引用和评论关联跳转。slug 只表示公开地址，改名不能改变编辑会话身份。前端解析固定路径形状，UUID 合法性由 API 校验，见 [ADR-0014](adr/0014-content-commits-and-stable-admin-identity.md)。

需要持续阅读或处理的失败、冲突、部分成功提示使用内联 `Alert` 的 `title`。临时成功反馈并非全站禁用：评论审核与回复目前使用上下文 `message.success`。反馈停留方式应取决于用户是否仍需采取动作。

命令式确认使用 `App.useApp().modal.confirm`，以继承主题与 locale；声明式 `Modal`、`Popconfirm` 也有现行使用。危险操作应说明影响并标出危险按钮。按钮名称与加载反馈服务于操作本身，测试在对应弹窗内定位，不为了避免同名断言限制产品文案。

## 表单与编辑会话

[文章编辑器](../apps/admin/src/screens/PostEditScreen.tsx)、[页面编辑器](../apps/admin/src/screens/PageEditScreen.tsx)和[设置屏](../apps/admin/src/screens/SettingsScreen.tsx)使用 Ant Design Form store 保存输入，渲染镜像由 `onValuesChange` 和统一写入函数同步。程序调用 `setFieldsValue` 时也要同步镜像，不假设它会触发用户输入回调。

- 用当前值与最近服务器基线比较 dirty，不能以 touched 状态代替。程序回填与用户实际修改不是同一件事。
- 发请求时保存提交快照。响应只覆盖等待期间没有继续修改的字段；保留新增输入，并明确提示其尚未保存。标签按集合比较，系列及序号作为关联字段一起处理。
- 编辑器记录已加载实体 ID；切换到另一 ID 尚未成功加载时禁止保存、发布等动作，不能把上一篇内容和版本提交到新地址。
- 创建成功先应用服务器结果和等待期间输入，再进入新 ID 地址，避免路由变化触发重载吞掉输入。
- 发布或撤回前如有未保存内容，先保存，再使用返回版本切换状态。状态响应只更新相应状态，不整体回填旧正文。
- 版本冲突保留本地输入。重新加载会丢弃本地值；确认覆盖则重新取得服务器版本后再提交，仍可能再次冲突。其他 409 如 slug 占用不能靠版本覆盖解决。

正文保持 Markdown，HTML 由服务端派生。需要对齐后端“字符”上限的字段使用 [codePointLength](../apps/admin/src/text.ts)，不能直接把 HTML `maxLength` 的 UTF-16 长度当作码点数。当前评论回复框仍使用 `maxLength`，不应据此声称所有输入已统一计数。

[未保存保护](../apps/admin/src/unsaved.tsx)由屏幕登记 dirty，外壳和屏内主动离开入口调用 `useLeaveConfirmation`；刷新或关闭标签页使用 `beforeunload`。保存、删除成功后的程序跳转不需要再次确认。当前不拦截浏览器前进、后退，也不代表每个短表单都登记了保护。

## 查询与缓存

[默认策略](../apps/admin/src/queryClient.ts)是 30 秒新鲜期、关闭窗口聚焦重取，只对 `ApiError.status >= 500` 最多重试两次；mutation 不自动重试。普通网络异常不满足这条 `ApiError` 判断。401 由 [API](../apps/admin/src/api.ts) 与[认证层](../apps/admin/src/auth.tsx)处理，Query 只负责不重试，尚未统一为 Query 级认证处理器。

列表、目录、设置和媒体主要使用 Query。Post/Page 详情仍直接请求并维护编辑基线；系列成员也由系列屏逐项加载。设置首次读取可初始化表单，后续后台重取不能无条件重新填表；版本冲突取服务器当前值时使用直接请求。

多数键集中在 `queryKeys`。评论列表当前使用 `['comments', page, status, post]`，评论开关使用 `['comment-policy', post ?? 'global']`，仍定义在组件内。新增或修改查询时要把实际影响结果的页码、过滤条件、资源身份放入键，并明确共享范围。

下表记录当前代码显式执行的缓存更新，便于定位调用点；它不是完整跨资源一致性的保证。

| 写入路径 | 当前更新范围 |
|---|---|
| Post 创建、保存、覆盖、发布、撤回 | `posts()`；先行保存与后续状态动作分别成功后分别失效 |
| Page 创建、保存、覆盖、发布、撤回、删除 | `pages()`；同样分步失效 |
| 文章列表移入回收站 | `posts()` 与 `trashAll()` |
| 回收站恢复 / 永久删除 | 都失效 `trashAll()`；恢复另失效 `posts()` |
| 媒体库上传、删除 | `mediaAll()` 前缀；删除被引用保护拒绝时另失效该资产 `mediaUsage(id)` |
| 标签、分类、系列写入 | 各自目录键；系列成员按屏内加载流程刷新 |
| 用户创建、角色分配或移除 | `users()` |
| 站点与主题设置保存 | 各自设置键；选择冲突快照重载时还会写入对应缓存 |
| 评论审核、删除、回复 | `['comments']` 前缀；审核或删除失败后也会重取 |
| 全站 / 单篇评论开关 | 成功响应写入自身 policy 键；失败使自身键失效 |

评论行为见 [CommentListScreen](../apps/admin/src/screens/CommentListScreen.tsx) 和 [CommentSwitch](../apps/admin/src/components/CommentSwitch.tsx)。后者使用 `useMutation`；复杂内容写入仍以显式函数编排，两种方式都必须保留版本和错误语义。

当前内容写入主要更新内容列表，尚未统一失效可能受影响的目录统计、媒体引用视图或评论关联信息。其他组件内直接上传也不能由“媒体库上传失效”推断为已处理。修改关联写入时应检查所有读取方，为需要即时一致的返回路径补充验证。

分页增删可能移动其他页，应按查询族失效。非活跃查询通常先标记过期，重新挂载时再取数。回收站使用 `keepPreviousData`，展示页码和行取同一份服务器数据，不把请求中的页码与上一页内容混用。`isPending`、`isFetching` 与成功空列表含义不同，禁用查询时也应先处理权限分支。

## 错误与写入结果

[apiError.ts](../apps/admin/src/apiError.ts)提供通用文案和管理权限文案，均可保留请求编号。登录、重新认证及具体业务错误应采用对应语义，不一律加权限前缀。接口字段、三态更新与错误码以[管理 API](admin-api.md)为准，不在 UI 层另定义一套协议。

取数错误与动作错误分开管理，避免后台重取覆盖刚刚发生的操作失败。写入成功而后续刷新失败时，要让用户知道写入已经生效，不能显示成从未保存；多步骤动作同样分别处理成功部分。自动重试不能代替用户处理版本冲突。

## 测试约定

测试以最终界面、有效载荷和是否允许写入为主要证据。测试与构建命令统一见[开发指南](development.md#检查与测试)。

- 异步挂载、表单校验、查询通知和弹窗收尾使用 `findBy*` 或 `waitFor` 等待可观察结果，不固定实现内部的调度次数。
- 验证请求期间输入时，先确认请求已发出，再改变输入并完成受控响应，断言新输入和未保存提示都保留。
- 验证写后缓存更新时，先访问列表形成旧缓存，再编辑并返回列表，断言新值；直接让初始查询返回新值无法证明失效有效。
- 请求次数只用于确有约束的行为，例如取消后不写、重复点击不重复提交、身份加载失败不写入。证明列表刷新应断言列表结果。
- 错误测试与重试测试分开控制：持续失败需覆盖所有尝试；不要让仅失败一次的 mock 被后续成功重试掩盖，也不要只为缩短等待把服务端故障改成语义不同的权限错误。
- 在当前对话框或控件范围内按角色、名称定位。Select、Modal 等按实际交互方式操作，不依赖内部 class、隐藏残留节点或动画结构。

可复用样例在 [editor.test.tsx](../apps/admin/tests/editor.test.tsx)、[listScreens.test.tsx](../apps/admin/tests/listScreens.test.tsx)、[settings.test.tsx](../apps/admin/tests/settings.test.tsx) 和 [CommentListScreen.test.tsx](../apps/admin/src/screens/CommentListScreen.test.tsx)。[testSetup.ts](../apps/admin/src/testSetup.ts)只补最小浏览器 API，不提供真实布局；组件测试不能替代构建后的浏览器交互与视觉核验。
