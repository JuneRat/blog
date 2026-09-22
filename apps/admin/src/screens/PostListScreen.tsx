import { useCallback, useEffect, useState } from "react";
import { ApiError, api, withRequestId } from "../api";
import { useAuth } from "../auth";
import { navigate, paths } from "../router";
import type { PostSummary } from "../types";

function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    const base = error.status === 403 ? `没有权限：${error.message}` : error.message;
    return withRequestId(base, error.requestId);
  }
  return error instanceof Error ? error.message : "未知错误";
}

export function PostListScreen() {
  const { me, logout, logoutError } = useAuth();
  const [posts, setPosts] = useState<PostSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const canCreate = me?.permissions.includes("post.create") ?? false;
  const canReadPages = me?.permissions.includes("page.read") ?? false;
  const canAdminister =
    (me?.permissions.includes("user.manage") ?? false) ||
    (me?.permissions.includes("role.manage") ?? false);

  const load = useCallback(async () => {
    setError(null);
    try {
      setPosts(await api.listPosts());
    } catch (e) {
      setPosts([]);
      setError(messageOf(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <div className="screen">
      <header className="topbar">
        <h1>我的文章</h1>
        <div className="topbar-actions">
          <span className="muted">{me?.permissions.length ?? 0} 项权限</span>
          {canReadPages && (
            <button type="button" className="button ghost" onClick={() => navigate(paths.pages)}>
              独立页面
            </button>
          )}
          <button type="button" className="button ghost" onClick={() => navigate(paths.tags)}>
            标签
          </button>
          {canAdminister && (
            <button type="button" className="button ghost" onClick={() => navigate(paths.users)}>
              用户与角色
            </button>
          )}
          {canCreate && (
            <button type="button" className="button" onClick={() => navigate(paths.newPost)}>
              新建草稿
            </button>
          )}
          <button type="button" className="button ghost" onClick={() => void logout()}>
            退出
          </button>
        </div>
      </header>

      {error !== null && <p className="error">{error}</p>}
      {logoutError !== null && <p className="error">{logoutError}</p>}
      {posts === null && <p className="muted">正在加载…</p>}
      {posts !== null && posts.length === 0 && error === null && (
        <p className="muted">还没有文章。{canCreate ? "点击「新建草稿」开始。" : ""}</p>
      )}

      {posts !== null && posts.length > 0 && (
        <table className="posts">
          <thead>
            <tr>
              <th>版本</th>
              <th>状态</th>
              <th>可见</th>
              <th>slug</th>
              <th>标题</th>
              <th>更新时间</th>
            </tr>
          </thead>
          <tbody>
            {posts.map((post) => (
              <tr key={post.id} onClick={() => navigate(paths.editPost(post.slug))}>
                <td>v{post.version}</td>
                <td>{post.status === "published" ? "已发布" : "草稿"}</td>
                <td>{post.visibility === "public" ? "公开" : "私有"}</td>
                <td>
                  <code>{post.slug}</code>
                </td>
                <td>{post.title || "（无标题）"}</td>
                <td className="muted">{post.updated_at}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
