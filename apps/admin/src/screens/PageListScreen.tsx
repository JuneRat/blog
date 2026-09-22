import { useCallback, useEffect, useState } from "react";
import { ApiError, api, withRequestId } from "../api";
import { useAuth } from "../auth";
import { navigate, paths } from "../router";
import type { PageSummary } from "../types";

function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    const base = error.status === 403 ? `没有权限：${error.message}` : error.message;
    return withRequestId(base, error.requestId);
  }
  return error instanceof Error ? error.message : "未知错误";
}

/** 独立页面列表：站点级 page.read，与文章列表（按作者）不同。 */
export function PageListScreen() {
  const { me, logout, logoutError } = useAuth();
  const [pages, setPages] = useState<PageSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const canCreate = me?.permissions.includes("page.create") ?? false;
  const canAdminister =
    (me?.permissions.includes("user.manage") ?? false) ||
    (me?.permissions.includes("role.manage") ?? false);

  const load = useCallback(async () => {
    setError(null);
    try {
      setPages(await api.listPages());
    } catch (e) {
      setPages([]);
      setError(messageOf(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <div className="screen">
      <header className="topbar">
        <h1>独立页面</h1>
        <div className="topbar-actions">
          <button type="button" className="button ghost" onClick={() => navigate(paths.list)}>
            我的文章
          </button>
          {canAdminister && (
            <button type="button" className="button ghost" onClick={() => navigate(paths.users)}>
              用户与角色
            </button>
          )}
          {canCreate && (
            <button type="button" className="button" onClick={() => navigate(paths.newPage)}>
              新建页面
            </button>
          )}
          <button type="button" className="button ghost" onClick={() => void logout()}>
            退出
          </button>
        </div>
      </header>

      <p className="muted">
        发布后通过根路径访问（如 /about）。slug 不能占用 admin、api、auth 等系统路径。
      </p>

      {error !== null && <p className="error">{error}</p>}
      {logoutError !== null && <p className="error">{logoutError}</p>}
      {pages === null && <p className="muted">正在加载…</p>}
      {pages !== null && pages.length === 0 && error === null && (
        <p className="muted">还没有页面。{canCreate ? "点击「新建页面」开始。" : ""}</p>
      )}

      {pages !== null && pages.length > 0 && (
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
            {pages.map((page) => (
              <tr key={page.id} onClick={() => navigate(paths.editPage(page.slug))}>
                <td>v{page.version}</td>
                <td>{page.status === "published" ? "已发布" : "草稿"}</td>
                <td>{page.visibility === "public" ? "公开" : "私有"}</td>
                <td>
                  <code>/{page.slug}</code>
                </td>
                <td>{page.title || "（无标题）"}</td>
                <td className="muted">{page.updated_at}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
