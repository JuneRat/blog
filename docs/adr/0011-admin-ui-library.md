# ADR-0011：后台 UI 迁移到 Ant Design v6

记录日期：2026-09-23。

状态：已采纳并实现。来源：用户明确「不用保留现有的 CSS 样式」，要求把后台从手写样式迁到成熟组件库。相关设计见 [身份、RBAC 与 SPA 后台](../identity-and-admin.md) 与 [架构总览 §7](../architecture.md)。

## 背景

后台 `apps/admin` 原本是「React + TypeScript + Vite + 手写 CSS」：`src/styles.css` 438 行、33 个选择器，全部组件自己写。缺口集中在三处：

- 11 处 `window.confirm`——原生弹窗无法表达危险色、也无法承载「不可恢复」「被 3 篇引用」这类需要看清的文案；
- 没有数据表格、上传进度、日期选择、表单校验，列表与表单全部手写；
- 提示分散在各屏自管的 `notice`/`error` 状态里，没有统一的展示层。

同时，公开站点是 Rust + MiniJinja SSR，与后台不共用主题模板（[架构总览 §7](../architecture.md)），后台可以独立选型。

## 决策

1. **选 Ant Design v6（React ≥18）**。理由：`Table`/`Form`/`Upload`/`DatePicker`/`message`/`Modal.confirm` 一次补齐全部缺口；zh_CN locale 是一等公民（分页、日期面板、上传、确认按钮文案开箱即中文）；v6 默认走 CSS 变量、`zeroRuntime` 可选，主题可控且与既有的 `:root` 变量思路同构；v6 起 React 19 不再需要 `@ant-design/v5-patch-for-react-19`。

2. **设计语言集中到 `src/providers.tsx` 的 seed token**（`colorPrimary`/`colorError`/`borderRadius`）。只设种子 token，不逐个覆盖组件 token：先吃默认视觉，真有具体不对的地方再加 `components` 覆盖，否则日后分不清是自调的还是组件本身的。自定义组件用 `theme.useToken()` 读同一套值。

3. **深浅色跟随系统**：`prefers-color-scheme` + `theme.darkAlgorithm`，与迁移前 `color-scheme: light dark` 的表现一致（没有手动开关）。注意 `color-scheme` 只影响浏览器原生控件，管不到 antd 组件，深色必须显式给算法。

4. **`AdminProviders`（`ConfigProvider` + antd `App`）放在 `App.tsx` 内，而不是 `main.tsx`**。测试是 `render(<App />)` 直接渲染根组件，放在这里才能让测试拿到与生产完全一致的主题、中文 locale 和 `App.useApp()` 上下文（`modal.confirm` 必须在 antd 的 `App` 之内才有效）。

5. **通知用内联 `Alert`（`title`），不用 message 吐司**。冲突、权限不足、「已发布；等待期间的新改动尚未保存」这类信息必须停留在屏幕上，3 秒后自动消失会让人来不及看清。

6. **确认弹窗统一用 `App.useApp().modal.confirm`**，确认按钮保持默认「确定」。不用静态方法 `Modal.confirm`：它不消费 ConfigProvider，主题与中文 locale 都不生效。确认按钮不改名为「删除」，否则与行内同名按钮一起会让按无障碍名定位的调用产生歧义。

7. **值的唯一来源是 antd Form 的 store，另存一份由 `onValuesChange` 维护的镜像供渲染与脏判断**。不使用 `Form.useWatch`：rc-field-form v1.8 把 watch 通知经 `MessageChannel` 批成宏任务，界面会比输入慢一拍，同步断言也读不到。

8. **保留全部领域逻辑**：
   - `formEquals` 基线比较不换成 `isFieldsTouched()`（`setFieldsValue` 也会把字段标记为 touched，加载服务器数据会让「未保存」一进页面就为真）；
   - `pickServer`/`mergeServer` 的「只覆盖用户在请求飞行期间没改过的字段」语义原样保留；
   - `expected_version` 乐观锁与 409 业务码分支不变；
   - 字符上限继续用 `codePointLength` 写自定义 `validator`：antd 的 `rules.max` 与 HTML `maxLength` 一样按 UTF-16 代码单元计数，emoji 会被算成 2（`src/text.ts` 记录的同一个坑）。

9. **导航收归 `AdminLayout`**（`Layout` + `Menu` + `Breadcrumb` + 退出登录），各屏不再自渲染 topbar 与「返回列表」按钮。菜单**不做权限过滤**：判定在后端用例与接口层，各屏也已按权限给出提示；前端隐藏入口只会让「有权限但菜单里没入口」变成新的故障面。

10. **按路由懒加载**（`React.lazy` + `Suspense`），让登录页与列表页不为编辑器、媒体库、设置付首屏体积。

11. **全局关闭两字中文按钮的自动空格**（`ConfigProvider button={{ autoInsertSpace: false }}`）。antd 默认只在「默认类型」按钮里插空格（「保存」渲染成「保 存」），text/link 类型不插，同类按钮文案不一致。

12. **暂不启用 `zeroRuntime`**：它需要预生成静态 CSS，等于给构建加一道工序；先用默认的 CSS 变量模式。

## 被否方案

- **Mantine 9**：CSS Modules + CSS 变量同样干净，且要求 React ≥19.2（本项目 19.3 满足）。但核心里没有数据网格（要另加 `mantine-datatable`）、Dropzone 只抓文件不给上传进度（要自己拼），中文文案与日期 locale 要自行接入，中文资料也少。在「不保留既有样式」的前提下，这些自行拼装的成本没有换来额外收益。
- **Headless（Base UI / Radix Primitives / React Aria）**：它们卖点是「行为给你、外观归你」。既然不再保留原样式，「外观归你」就只剩成本，等于把 antd 已经写好的一整套组件重写一遍。
- **后台元框架（Ant Design Pro / refine / react-admin / AdminJS）**：见 [ADR 之外的评估结论](../identity-and-admin.md)。关键事实：这些框架的收益集中在项目开局（脚手架、资源抽象、权限与布局），而本项目已有 13 个屏与 1535 行测试；写路径语义（乐观锁、业务错误码、内存 CSRF、按权限点判定）塞进 Data Provider 适配层只会更差；AdminJS 需要 Node 进程接 ORM，而本项目是 Rust + SQLx；Ant Design Pro 的载体是 Umi Max + Turbopack，与现有 Vite（`base: "/admin/"`）+ Rust `mount_admin_spa` 深链回退是两套构建体系。
- **Tailwind / shadcn/ui**：等于再引入一套与 antd 重叠的样式体系；shadcn 的「组件源码拷进仓库」模式本身契合本仓库的备份与评审习惯，但前提是先接受 Tailwind。
- **自研富文本编辑器**：正文仍是 Markdown，公开站点由 `pulldown-cmark` 渲染，编辑器的选型（例如前后端渲染一致性）单独决策，不与 UI 库绑定。
- **全量替换成 antd 的 `Select` 做标签多选**：改用 `Checkbox.Group`。它同样渲染原生 checkbox，既得到 antd 的外观与 `Form.Item` 集成，又保住了按标签名定位的既有测试与键盘可达性。

## 代价与限制

- **测试必须跟着改**（已全部完成）：`window.confirm` → 点「确定」（6 处桩）；同步断言 → `findBy*`/`waitFor`（antd 的校验、提交与弹窗关闭都是异步的）；「点保存后立刻改输入」要先等请求真正发出，否则不再是「请求飞行期间」；antd `Select` 不能用 `fireEvent.change` 赋值，要 `mouseDown` + 点选项；关闭后的 Modal 仍留在 DOM 里，确认与取消要拆成不同用例；屏幕按路由懒加载后，首次渲染是 Suspense 占位，必须先 `await` 真实屏幕挂载。
- **首屏体积上升**：靠按路由懒加载与 `zeroRuntime`（未来）约束；`dist` 现在是多个 chunk。构建仍会报一条「有 chunk 超过 500 kB」的提示：那是 antd 核心与共享运行时的合并块（被入口 modulepreload），后台单次加载、带指纹 immutable 缓存，暂不为此手写 chunk 拆分配置（rolldown 的分包 API 与本仓库现有构建还未验证）。
- **antd 的弃用面要跟**：6.6.5 已弃用 `Alert` 的 `message`（改用 `title`）、`List`（指向新的 `Listy`）。本仓库统一用 `title`，列表用 `Table`。
- **提交按钮用文案切换（「处理中…」/正常文案）而不是 `Button` 的 `loading`**。antd 的 loading 图标是 motion 包裹的，动画结束后仍留在 DOM 里（jsdom 里动画不会结束，常驻），其 `role="img" aria-label="loading"` 会污染按钮的无障碍名——读屏会念出多余的 "loading"，按名字定位也会失效。文案切换没有这个残留，也与迁移前「处理中…」的表现一致。
- **关闭后的 `Modal` 仍留在 DOM 里**：同一用例里连续打开两次确认弹窗会同时匹配到两个「确定」。确认与取消因此拆成不同用例。
- **视觉随 antd 走**：用户明确不保留原样式；主题只通过 token 与 `classNames`/`styles` 语义槽位调整，**不改 antd 内部 DOM 结构**（升级时这类选择器会脆）。
- **深浅色不再自动生效**：必须显式给 `algorithm`；当前只有「跟随系统」，没有手动开关（与迁移前一致）。
- **四个原本没有测试覆盖的屏**（PostList、PageList、PostTrash、RoleList）靠类型检查与冒烟验证保障，风险高于有测试的屏。
