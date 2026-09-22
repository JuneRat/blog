import { useCallback, useEffect, useState } from "react";
import { ApiError, api, withRequestId } from "../api";
import { useAuth } from "../auth";
import { navigate, paths } from "../router";
import type { RoleSummary } from "../types";

/**
 * 角色目录：列出内置角色与授权数量。
 *
 * 当前只读：角色分配在「用户与角色」界面完成，自定义角色的创建/改名/授权编辑
 * 尚未提供对应用例，因此这里不放置任何写控件——不存在的后端能力不能靠前端假装。
 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return withRequestId(error.message, error.requestId);
  }
  return error instanceof Error ? error.message : "未知错误";
}

export function RoleListScreen() {
  const { me, logout, logoutError } = useAuth();
  const [roles, setRoles] = useState<RoleSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const canAdminister =
    (me?.permissions.includes("role.manage") ?? false) ||
    (me?.permissions.includes("user.manage") ?? false);

  const load = useCallback(async () => {
    setError(null);
    try {
      setRoles(await api.listRoles());
    } catch (e) {
      setRoles([]);
      setError(messageOf(e));
    }
  }, []);

  useEffect(() => {
    if (canAdminister) void load();
  }, [canAdminister, load]);

  if (!canAdminister) {
    return (
      <div className="screen">
        <header className="topbar">
          <h1>角色目录</h1>
          <div className="topbar-actions">
            <button type="button" className="button ghost" onClick={() => navigate(paths.list)}>
              我的文章
            </button>
            <button type="button" className="button ghost" onClick={() => void logout()}>
              退出
            </button>
          </div>
        </header>
        <p className="warning">当前账号没有 role.manage 或 user.manage 权限，无法查看角色。</p>
      </div>
    );
  }

  return (
    <div className="screen">
      <header className="topbar">
        <h1>角色目录</h1>
        <div className="topbar-actions">
          <button type="button" className="button ghost" onClick={() => navigate(paths.list)}>
            我的文章
          </button>
          <button type="button" className="button ghost" onClick={() => navigate(paths.users)}>
            用户与角色
          </button>
          <button type="button" className="button ghost" onClick={() => void logout()}>
            退出
          </button>
        </div>
      </header>

      <p className="muted">
        内置角色由初始化种子保留，不能通过普通 API 创建、改名或删除；角色分配在「用户与角色」界面完成。
      </p>

      {error !== null && <p className="error">{error}</p>}
      {logoutError !== null && <p className="error">{logoutError}</p>}
      {roles === null && <p className="muted">正在加载…</p>}
      {roles !== null && roles.length === 0 && error === null && <p className="muted">没有角色。</p>}

      {roles !== null && roles.length > 0 && (
        <table className="posts">
          <thead>
            <tr>
              <th>slug</th>
              <th>名称</th>
              <th>类型</th>
              <th>权限数</th>
              <th>说明</th>
            </tr>
          </thead>
          <tbody>
            {roles.map((role) => (
              <tr key={role.slug}>
                <td>
                  <code>{role.slug}</code>
                </td>
                <td>{role.name}</td>
                <td>
                  <span className="badge">{role.builtin ? "内置" : "自定义"}</span>
                </td>
                <td>{role.permission_count}</td>
                <td className="muted">{role.description ?? "—"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
