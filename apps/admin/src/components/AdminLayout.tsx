import { Alert, Avatar, Breadcrumb, Button, Flex, Layout, Menu, Space, Typography, theme } from "antd";
import type { MenuProps } from "antd";
import type { ReactNode } from "react";
import { lazy, Suspense, useState } from "react";
import { useAuth } from "../auth";
import { navigate, paths, useRoute } from "../router";
import type { Route } from "../router";
import { useLeaveConfirmation } from "../unsaved";

const PasswordChangeModal = lazy(() => import("./PasswordChangeModal").then(module => ({ default: module.PasswordChangeModal })));
const AvatarChangeModal = lazy(() => import("./AvatarChangeModal").then(module => ({ default: module.AvatarChangeModal })));

const { Header, Sider, Content } = Layout;

/**
 * 后台外壳：左侧导航 + 顶部面包屑与账号操作 + 内容区。
 *
 * 迁移前每个屏各自渲染一个 `<header class="topbar">` 和「返回列表」按钮，
 * 导航靠来回跳；现在统一收编到这里，屏只负责自己的内容。
 *
 * 菜单**不做权限过滤**：后端用例与接口层才是权限判定处，各屏也已按权限
 * 显示自己的提示（如角色目录）。前端隐藏入口只会让「有权限但菜单里没入口」
 * 变成新的故障面，与迁移前的行为也不一致。
 */
const NAV: MenuProps["items"] = [
  {
    key: "content",
    label: "内容",
    children: [
      { key: paths.list, label: "文章" },
      { key: paths.comments, label: "评论管理" },
      { key: paths.pages, label: "独立页面" },
      { key: paths.media, label: "媒体库" },
      { key: paths.postTrash, label: "回收站" },
    ],
  },
  {
    key: "taxonomy",
    label: "目录",
    children: [
      { key: paths.tags, label: "标签" },
      { key: paths.categories, label: "分类" },
      { key: paths.series, label: "系列" },
    ],
  },
  {
    key: "system",
    label: "系统",
    children: [
      { key: paths.users, label: "用户与角色" },
      { key: paths.roles, label: "角色" },
      { key: paths.settings, label: "站点设置" },
      { key: paths.themes, label: "主题管理" },
      { key: paths.plugins, label: "插件管理" },
      { key: paths.tasks, label: "任务管理" },
      { key: paths.auditLogs, label: "审计日志" },
    ],
  },
];

/** 路由 → 面包屑末级标题。 */
const TITLES: Record<Route["name"], string> = {
  list: "文章",
  comments: "评论管理",
  postTrash: "文章回收站",
  postNew: "新建草稿",
  postEdit: "编辑文章",
  pageList: "独立页面",
  pageNew: "新建页面",
  pageEdit: "编辑页面",
  tagList: "标签",
  mediaLibrary: "媒体库",
  categoryList: "分类",
  seriesList: "系列",
  pageTrash: "页面回收站",
  userList: "用户与角色",
  profile: "个人资料",
  roleList: "角色目录",
  settings: "站点设置",
  themes: "主题管理",
  plugins: "插件管理",
  tasks: "任务管理",
  auditLogs: "审计日志",
  invalid: "地址无法识别",
};

/** 路由 → 菜单选中项（编辑页归属它所属的列表）。 */
function selectedKey(route: Route): string {
  switch (route.name) {
    case "comments":
      return paths.comments;
    case "list":
    case "postTrash":
    case "postNew":
    case "postEdit":
      return paths.list;
    case "pageList":
    case "pageNew":
    case "pageEdit":
      return paths.pages;
    case "tagList":
      return paths.tags;
    case "categoryList":
      return paths.categories;
    case "pageTrash":
      return paths.pages;
    case "seriesList":
      return paths.series;
    case "mediaLibrary":
      return paths.media;
    case "userList":
      return paths.users;
    case "roleList":
      return paths.roles;
    case "settings":
      return paths.settings;
    case "themes":
      return paths.themes;
    case "plugins":
      return paths.plugins;
    case "tasks":
      return paths.tasks;
    case "auditLogs":
      return paths.auditLogs;
    case "invalid":
    case "profile":
      return "";
  }
}

export function AdminLayout({ children, readerOnly = false }: { children: ReactNode; readerOnly?: boolean }) {
  const { token } = theme.useToken();
  const route = useRoute();
  // 测试里 mock 的 auth 只给 status/me，缺少的字段用可选调用兜住，
  // 免得外壳把整屏带崩（真实 AuthProvider 一定提供这些方法）。
  const { me, logout, logoutError } = useAuth();
  const confirmLeave = useLeaveConfirmation();
  const [passwordOpen, setPasswordOpen] = useState(false);
  const [avatarOpen, setAvatarOpen] = useState(false);

  /**
   * 菜单导航：离开前先过统一的确认口径。
   *
   * 「已经在目标页」用**真实路径**判断，不能用菜单选中项（`selectedKey`）：
   * 文章编辑页与回收站都映射到「文章」这一项，用选中项判断会让
   * 「编辑页 → 文章列表」这条最常用的返回路径直接失效（点了没反应）。
   */
  function goTo(key: string): void {
    if (key === window.location.pathname) return;
    confirmLeave(() => navigate(key));
  }

  return (
    <Layout style={{ minHeight: "100vh" }}>
      {!readerOnly && <Sider theme="light" breakpoint="lg" collapsedWidth={0} width={208}>
        <div style={{ padding: "18px 20px" }}>
          <Typography.Text strong>博客后台</Typography.Text>
        </div>
        <Menu
          mode="inline"
          items={NAV}
          selectedKeys={[selectedKey(route)]}
          defaultOpenKeys={["content", "taxonomy", "system"]}
          onClick={({ key }) => goTo(key)}
        />
      </Sider>}
      <Layout>
        <Header
          style={{
            background: token.colorBgContainer,
            paddingInline: 24,
            paddingBlock: 12,
            height: "auto",
            minHeight: 64,
            lineHeight: "normal",
            display: "flex",
            flexWrap: "wrap",
            gap: 12,
            alignItems: "center",
            justifyContent: "space-between",
            borderBottom: `1px solid ${token.colorBorderSecondary}`,
          }}
        >
          <Breadcrumb items={readerOnly ? [{ title: "个人中心" }] : [{ title: "博客后台" }, { title: TITLES[route.name] }]} />
          <Flex gap={12} align="center" wrap style={{ maxWidth: "100%" }}>
            <Button type="link" href="/" target="_blank" style={{ paddingInline: 8 }}>
              查看站点 ↗
            </Button>
            {/* 头像入口：显示当前头像，点击直接打开自助更换弹窗。 */}
            <Button
              type="text"
              onClick={() => setAvatarOpen(true)}
              aria-label="更换头像"
              title="更换头像"
              style={{ paddingInline: 4 }}
            >
              <Flex gap={8} align="center">
                <Avatar size={28} src={me?.avatar_url ?? undefined}>
                  {(me?.display_name ?? me?.username ?? "?").slice(0, 1)}
                </Avatar>
                <span style={{ fontWeight: 500 }}>
                  {me?.display_name ?? me?.username ?? "我的账号"}
                </span>
              </Flex>
            </Button>
            {/* 顶栏用户操作：采用轻量 borderless 按钮与紧凑容器，解决孤立灰色按钮视觉杂乱问题 */}
            <Space.Compact
              style={{
                background: token.colorFillQuaternary,
                borderRadius: token.borderRadiusSM,
                padding: "2px 4px",
              }}
            >
              <Button type="text" size="small" onClick={() => goTo(paths.profile)}>
                个人资料
              </Button>
              <Button type="text" size="small" onClick={() => setPasswordOpen(true)}>
                修改密码
              </Button>
              <Button
                type="text"
                size="small"
                danger
                onClick={() => confirmLeave(() => void logout?.(), "放弃修改并退出")}
              >
                退出登录
              </Button>
            </Space.Compact>
          </Flex>
        </Header>
        <Content style={{ padding: "16px 24px 48px" }}>
          <div style={{ maxWidth: 1400, margin: "0 auto", width: "100%" }}>
            {/* 退出失败时会话仍然有效，不能假装已退出：沿用 auth 的 logoutError 明确提示。 */}
            {logoutError != null && (
              <Alert type="error" showIcon title={logoutError} style={{ marginBottom: 16 }} />
            )}
            {children}
          </div>
        </Content>
      </Layout>
      <Suspense fallback={<Typography.Text role="status">正在加载账号设置…</Typography.Text>}>
        {passwordOpen && <PasswordChangeModal open onClose={() => setPasswordOpen(false)} />}
        {avatarOpen && <AvatarChangeModal onClose={() => setAvatarOpen(false)} />}
      </Suspense>
    </Layout>
  );
}
