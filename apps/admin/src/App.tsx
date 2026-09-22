import { useAuth } from "./auth";
import { navigate, paths, useRoute } from "./router";
import { LoginScreen } from "./screens/LoginScreen";
import { PageEditScreen } from "./screens/PageEditScreen";
import { PageListScreen } from "./screens/PageListScreen";
import { PostEditScreen } from "./screens/PostEditScreen";
import { PostListScreen } from "./screens/PostListScreen";
import { TagListScreen } from "./screens/TagListScreen";
import { RoleListScreen } from "./screens/RoleListScreen";
import { UserListScreen } from "./screens/UserListScreen";

export function App() {
  const { status } = useAuth();
  const route = useRoute();

  if (status === "loading") {
    return (
      <div className="screen">
        <p className="muted">正在校验会话…</p>
      </div>
    );
  }
  if (status !== "authenticated") {
    return <LoginScreen />;
  }
  if (route.name === "invalid") {
    return (
      <div className="screen">
        <h1>地址无法识别</h1>
        <p className="muted">这个后台地址不存在，或包含无法解析的字符。</p>
        <p>
          <button type="button" className="button" onClick={() => navigate(paths.list)}>
            返回列表
          </button>
        </p>
      </div>
    );
  }
  // 编辑器不按 slug 加 key：改名时 slug 变化只更新地址，组件内已合并好的表单不应被重载覆盖；
  // 真正切换到另一篇内容时，编辑屏自己会按 slug 变化重新加载。
  if (route.name === "postEdit") {
    return <PostEditScreen slug={route.slug} />;
  }
  if (route.name === "postNew") {
    return <PostEditScreen slug={null} />;
  }
  if (route.name === "pageList") {
    return <PageListScreen />;
  }
  if (route.name === "pageEdit") {
    return <PageEditScreen slug={route.slug} />;
  }
  if (route.name === "pageNew") {
    return <PageEditScreen slug={null} />;
  }
  // 路由守卫只改善体验：真正的权限判断在用例与接口层，无权限时后端返回 403。
  if (route.name === "tagList") {
    return <TagListScreen />;
  }
  if (route.name === "userList") {
    return <UserListScreen />;
  }
  if (route.name === "roleList") {
    return <RoleListScreen />;
  }
  return <PostListScreen />;
}
