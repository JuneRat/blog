import { Alert, Avatar, Breadcrumb, Button, Flex, Layout, Menu, Modal, Typography } from "antd";
import type { MenuProps } from "antd";
import type { ReactNode } from "react";
import { useState } from "react";
import { api } from "../api";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { PasswordChangeModal } from "./PasswordChangeModal";
import { CoverPicker } from "./CoverPicker";
import { navigate, paths, useRoute } from "../router";
import type { Route } from "../router";
import { useLeaveConfirmation } from "../unsaved";

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
      { key: paths.auditLogs, label: "审计日志" },
    ],
  },
];

/** 路由 → 面包屑末级标题。 */
const TITLES: Record<Route["name"], string> = {
  list: "我的文章",
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
  roleList: "角色目录",
  settings: "站点设置",
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
    case "auditLogs":
      return paths.auditLogs;
    case "invalid":
      return "";
  }
}

export function AdminLayout({ children }: { children: ReactNode }) {
  const route = useRoute();
  // 测试里 mock 的 auth 只给 status/me，缺少的字段用可选调用兜住，
  // 免得外壳把整屏带崩（真实 AuthProvider 一定提供这些方法）。
  const { me, refresh, logout, logoutError } = useAuth();
  const confirmLeave = useLeaveConfirmation();
  const [passwordOpen, setPasswordOpen] = useState(false);
  /** 头像弹窗：选择值单独存一份，点「保存」前不影响头部显示。 */
  const [avatarOpen, setAvatarOpen] = useState(false);
  const [avatarValue, setAvatarValue] = useState<string | null>(null);
  const [avatarBusy, setAvatarBusy] = useState(false);
  const [avatarError, setAvatarError] = useState<string | null>(null);
  const canReadMedia = me?.permissions.includes("media.read") ?? false;
  const canUploadMedia = me?.permissions.includes("media.upload") ?? false;

  /** 打开头像弹窗：以当前头像为起点，取消不改动任何数据。 */
  function openAvatar(): void {
    setAvatarValue(me?.avatar_media_id ?? null);
    setAvatarError(null);
    setAvatarOpen(true);
  }

  /** 保存头像：本人自助接口；成功后刷新 `/me` 让头部立即更新。 */
  async function saveAvatar(): Promise<void> {
    setAvatarBusy(true);
    setAvatarError(null);
    try {
      await api.setOwnAvatar(avatarValue);
      await refresh?.();
      setAvatarOpen(false);
    } catch (e) {
      setAvatarError(permissionMessageOf(e));
    } finally {
      setAvatarBusy(false);
    }
  }

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
      <Sider theme="light" breakpoint="lg" collapsedWidth={0} width={208}>
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
      </Sider>
      <Layout>
        <Header
          style={{
            paddingInline: 24,
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
          }}
        >
          <Breadcrumb items={[{ title: "博客后台" }, { title: TITLES[route.name] }]} />
          <Flex gap={8} align="center">
            <Button type="link" href="/" target="_blank">
              查看站点
            </Button>
            {/* 头像入口：显示当前头像，点击打开自助更换弹窗。 */}
            <Button type="text" onClick={openAvatar} style={{ paddingInline: 4 }}>
              <Flex gap={8} align="center">
                <Avatar size={28} src={me?.avatar_url ?? undefined}>
                  {(me?.display_name ?? me?.username ?? "?").slice(0, 1)}
                </Avatar>
                <span>更换头像</span>
              </Flex>
            </Button>
            <Button onClick={() => setPasswordOpen(true)}>修改密码</Button>
            <Button onClick={() => confirmLeave(() => void logout?.(), "放弃修改并退出")}>
              退出登录
            </Button>
          </Flex>
        </Header>
        <Content style={{ padding: "8px 24px 48px" }}>
          <div style={{ maxWidth: 1040, margin: "0 auto" }}>
            {/* 退出失败时会话仍然有效，不能假装已退出：沿用 auth 的 logoutError 明确提示。 */}
            {logoutError != null && (
              <Alert type="error" showIcon title={logoutError} style={{ marginBottom: 16 }} />
            )}
            {children}
          </div>
        </Content>
      </Layout>
      <PasswordChangeModal open={passwordOpen} onClose={() => setPasswordOpen(false)} />
      {/* 只挂载打开的弹窗：关闭即卸载，避免残留列表/文件输入。 */}
      {avatarOpen && (
        <Modal
          title="更换头像"
          open
          okText="保存"
          cancelText="取消"
          confirmLoading={avatarBusy}
          onOk={() => void saveAvatar()}
          onCancel={() => setAvatarOpen(false)}
          destroyOnHidden
        >
          <CoverPicker
            value={avatarValue}
            onChange={setAvatarValue}
            // 服务端下发的地址只对「当前头像 id」有效：一旦在弹窗里换了图，
            // 必须回退到 mediaUrl(value) 显示新图，否则预览停在旧头像上。
            currentUrl={
              avatarValue === (me?.avatar_media_id ?? null) ? (me?.avatar_url ?? null) : null
            }
            canReadMedia={canReadMedia}
            canUploadMedia={canUploadMedia}
            disabled={avatarBusy}
            label="头像"
          />
          {avatarError !== null && (
            <Alert type="error" showIcon title={avatarError} style={{ marginTop: 12 }} />
          )}
        </Modal>
      )}
    </Layout>
  );
}
