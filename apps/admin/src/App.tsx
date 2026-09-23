import { lazy, Suspense } from "react";
import { Button, Result, Spin, Typography } from "antd";
import { useAuth } from "./auth";
import { AdminLayout } from "./components/AdminLayout";
import { AdminProviders } from "./providers";
import { navigate, paths, useRoute } from "./router";
import { LoginScreen } from "./screens/LoginScreen";
import { UnsavedChangesProvider } from "./unsaved";

/**
 * 按路由懒加载屏幕：编辑器、媒体库、设置是三个大块，
 * 让登录页与列表页不必为它们付首屏体积（antd 本身就不小）。
 *
 * 命名导出用 `.then()` 转成 default，避免为了懒加载改动各屏的导出形状。
 */
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
const RoleListScreen = lazy(() =>
  import("./screens/RoleListScreen").then((m) => ({ default: m.RoleListScreen })),
);
const SettingsScreen = lazy(() =>
  import("./screens/SettingsScreen").then((m) => ({ default: m.SettingsScreen })),
);

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
  return (
    <AdminProviders>
      {/* 未保存改动登记处：屏幕登记、外壳在导航前确认（见 src/unsaved.tsx）。 */}
      <UnsavedChangesProvider>
        <AdminRoutes />
      </UnsavedChangesProvider>
    </AdminProviders>
  );
}

function AdminRoutes() {
  const auth = useAuth();
  const route = useRoute();

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

  return (
    <AdminLayout>
      <Suspense fallback={<Loading />}>
        {route.name === "invalid" ? (
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
        ) : route.name === "postEdit" ? (
          // 编辑器不按 slug 加 key：改名时 slug 变化只更新地址，组件内已合并好的表单
          // 不应被重载覆盖；真正切换到另一篇内容时，编辑屏自己会按 slug 变化重新加载。
          <PostEditScreen slug={route.slug} />
        ) : route.name === "postNew" ? (
          <PostEditScreen slug={null} />
        ) : route.name === "postTrash" ? (
          <PostTrashScreen />
        ) : route.name === "pageList" ? (
          <PageListScreen />
        ) : route.name === "pageEdit" ? (
          <PageEditScreen slug={route.slug} />
        ) : route.name === "pageNew" ? (
          <PageEditScreen slug={null} />
        ) : route.name === "tagList" ? (
          <TagListScreen />
        ) : route.name === "mediaLibrary" ? (
          <MediaLibraryScreen />
        ) : route.name === "categoryList" ? (
          <CategoryListScreen />
        ) : route.name === "seriesList" ? (
          <SeriesListScreen />
        ) : route.name === "userList" ? (
          <UserListScreen />
        ) : route.name === "roleList" ? (
          <RoleListScreen />
        ) : route.name === "settings" ? (
          <SettingsScreen />
        ) : (
          <PostListScreen />
        )}
      </Suspense>
    </AdminLayout>
  );
}
