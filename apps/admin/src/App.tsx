import { lazy, Suspense, useEffect, useState } from "react";
import { Button, Result, Spin, Typography } from "antd";
import { useAuth } from "./auth";
import { AdminLayout } from "./components/AdminLayout";
import { AdminProviders } from "./providers";
import { navigate, paths, useRoute } from "./router";
import { PasswordRecoveryScreen } from "./screens/PasswordRecoveryScreen";
import { LoginScreen } from "./screens/LoginScreen";
import { UnsavedChangesProvider } from "./unsaved";

/**
 * 按路由懒加载屏幕：编辑器、媒体库、设置是三个大块，
 * 让登录页与列表页不必为它们付首屏体积（antd 本身就不小）。
 *
 * 命名导出用 `.then()` 转成 default，避免为了懒加载改动各屏的导出形状。
 */
const CommentListScreen = lazy(() => import("./screens/CommentListScreen").then(m => ({default:m.CommentListScreen})));
const PostListScreen = lazy(() =>
  import("./screens/PostListScreen").then((m) => ({ default: m.PostListScreen })),
);
const PostEditScreen = lazy(() =>
  import("./screens/PostEditScreen").then((m) => ({ default: m.PostEditScreen })),
);
const PostTrashScreen = lazy(() =>
  import("./screens/PostTrashScreen").then((m) => ({ default: m.PostTrashScreen })),
);
const PageListScreen = lazy(() =>
  import("./screens/PageListScreen").then((m) => ({ default: m.PageListScreen })),
);
const PageEditScreen = lazy(() =>
  import("./screens/PageEditScreen").then((m) => ({ default: m.PageEditScreen })),
);
const TagListScreen = lazy(() =>
  import("./screens/TagListScreen").then((m) => ({ default: m.TagListScreen })),
);
const CategoryListScreen = lazy(() =>
  import("./screens/CategoryListScreen").then((m) => ({ default: m.CategoryListScreen })),
);
const SeriesListScreen = lazy(() =>
  import("./screens/SeriesListScreen").then((m) => ({ default: m.SeriesListScreen })),
);
const MediaLibraryScreen = lazy(() =>
  import("./screens/MediaLibraryScreen").then((m) => ({ default: m.MediaLibraryScreen })),
);
const UserListScreen = lazy(() =>
  import("./screens/UserListScreen").then((m) => ({ default: m.UserListScreen })),
);
const ProfileScreen = lazy(() =>
  import("./screens/ProfileScreen").then((m) => ({ default: m.ProfileScreen })),
);
const RoleListScreen = lazy(() =>
  import("./screens/RoleListScreen").then((m) => ({ default: m.RoleListScreen })),
);
const SettingsScreen = lazy(() =>
  import("./screens/SettingsScreen").then((m) => ({ default: m.SettingsScreen })),
);
const AuditLogScreen = lazy(() =>
  import("./screens/AuditLogScreen").then((m) => ({ default: m.AuditLogScreen })),
);
const TaskManagementScreen = lazy(() => import("./screens/TaskManagementScreen").then(m => ({ default: m.TaskManagementScreen })));
const PluginManagementScreen = lazy(() => import("./screens/PluginManagementScreen").then(m => ({ default: m.PluginManagementScreen })));
const ThemeManagementScreen = lazy(() => import("./screens/ThemeManagementScreen").then(m => ({ default: m.ThemeManagementScreen })));

function Loading(): React.ReactNode {
  return (
    <div style={{ padding: 48, textAlign: "center" }}>
      <Spin size="large" />
      <Typography.Paragraph type="secondary" style={{ marginTop: 16 }}>
        正在加载…
      </Typography.Paragraph>
    </div>
  );
}

/**
 * 应用根组件。
 *
 * `AdminProviders` 放在这里而不是 `main.tsx`：测试直接 `render(<App />)`，
 * 这样它们拿到的主题、中文 locale 与 antd App 上下文和生产完全一致
 * （`App.useApp()` 的 modal.confirm 必须处在 antd 的 App 内才有效）。
 */
export function App() {
  const auth = useAuth();
  // Keep the bearer above the session-keyed provider: /me completing must not
  // discard it after the fragment has already been removed from history.
  const [resetToken] = useState(() => new URLSearchParams(window.location.hash.slice(1)).get("password-reset"));
  useEffect(() => {
    if (resetToken !== null) window.history.replaceState(window.history.state, "", window.location.pathname + window.location.search);
  }, [resetToken]);
  // A session's queries and editor state must not survive a different identity.
  const sessionKey = auth.status === "authenticated" ? auth.me?.user_id : auth.status;
  return (
    <AdminProviders key={sessionKey}>
      {/* 未保存改动登记处：屏幕登记、外壳在导航前确认（见 src/unsaved.tsx）。 */}
      <UnsavedChangesProvider>
        <AdminRoutes resetToken={resetToken} />
      </UnsavedChangesProvider>
    </AdminProviders>
  );
}

function AdminRoutes({ resetToken }: { resetToken: string | null }) {
  const auth = useAuth();
  const route = useRoute();
  if (resetToken !== null) return <PasswordRecoveryScreen token={resetToken} onBack={() => window.location.assign("/admin/")} />;


  if (auth.status === "loading") {
    return (
      <div style={{ padding: 48, textAlign: "center" }}>
        <Spin size="large" />
        <Typography.Paragraph type="secondary" style={{ marginTop: 16 }}>
          正在校验会话…
        </Typography.Paragraph>
      </div>
    );
  }
  if (auth.status !== "authenticated") {
    return <LoginScreen />;
  }

  const readerOnly = auth.me?.permissions.length === 0;
  return (
    <AdminLayout readerOnly={readerOnly}>
      <Suspense fallback={<Loading />}>
        {readerOnly ? (
          <><Typography.Paragraph>欢迎回来！你可以浏览文章并发表评论。</Typography.Paragraph><Button href="/">浏览文章</Button><ProfileScreen /></>
        ) : route.name === "invalid" ? (
          <Result
            status="warning"
            title="地址无法识别"
            subTitle="这个后台地址不存在，或包含无法解析的字符。"
            extra={
              <Button type="primary" onClick={() => navigate(paths.list)}>
                返回列表
              </Button>
            }
          />
        ) : route.name === "comments" ? (
          <CommentListScreen />
        ) : route.name === "postEdit" ? (
          // 创建后由编辑器保留已经合并的输入；切换实体按稳定 ID 重新加载。
          <PostEditScreen id={route.id} />
        ) : route.name === "postNew" ? (
          <PostEditScreen id={null} />
        ) : route.name === "postTrash" ? (
          <PostTrashScreen />
        ) : route.name === "pageList" ? (
          <PageListScreen />
        ) : route.name === "pageEdit" ? (
          <PageEditScreen id={route.id} />
        ) : route.name === "pageNew" ? (
          <PageEditScreen id={null} />
        ) : route.name === "tagList" ? (
          <TagListScreen />
        ) : route.name === "mediaLibrary" ? (
          <MediaLibraryScreen />
        ) : route.name === "categoryList" ? (
          <CategoryListScreen />
        ) : route.name === "pageTrash" ? (
          <PostTrashScreen kind="page" />
        ) : route.name === "seriesList" ? (
          <SeriesListScreen />
        ) : route.name === "userList" ? (
          <UserListScreen />
        ) : route.name === "profile" ? (
          <ProfileScreen />
        ) : route.name === "roleList" ? (
          <RoleListScreen />
        ) : route.name === "settings" ? (
          <SettingsScreen />
        ) : route.name === "themes" ? (
          <ThemeManagementScreen />
        ) : route.name === "plugins" ? (
          <PluginManagementScreen />
        ) : route.name === "tasks" ? (
          <TaskManagementScreen />
        ) : route.name === "auditLogs" ? (
          <AuditLogScreen />
        ) : (
          <PostListScreen />
        )}
      </Suspense>
    </AdminLayout>
  );
}
