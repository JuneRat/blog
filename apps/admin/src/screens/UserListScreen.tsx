import { useCallback, useEffect, useState } from "react";
import type { FormEvent } from "react";
import { ApiError, api, withRequestId } from "../api";
import { useAuth } from "../auth";
import { navigate, paths } from "../router";
import type { AdminUser, RoleSummary } from "../types";

/// 账号列表一次读取的上限（与后端 `ADMIN_USER_PAGE_MAX` 对齐）。
const USER_PAGE_LIMIT = 200;

/**
 * 账号管理界面：列出账号、创建账号、分配/移除角色。
 *
 * 权限由后端执行，这里只据 `/me` 的权限决定「展示哪些控件」，避免把 403
 * 当作正常交互；路由守卫同理只改善体验。所有写操作都走受保护的 API
 * （会话 + CSRF），并按业务码给出精确文案：
 * - `username_taken` / `email_taken`：创建表单字段级错误；
 * - `last_owner`：解释最后一个可登录 Owner 为何不能被移除；
 * - `forbidden`：可能是缺少 ownership.manage 或超出委派上限。
 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    switch (error.code) {
      case "username_taken":
        return "用户名已被占用，请换一个。";
      case "email_taken":
        return "邮箱已被其他账号使用，请换一个或留空。";
      case "last_owner":
        return "这是最后一个可登录的 Owner，不能移除其 Owner 角色；请先给另一个账号授予 Owner 并绑定登录方式。";
      case "forbidden":
        return withRequestId(
          "没有权限执行该操作：可能缺少 ownership.manage，或超出了你的委派上限（不能授予自己不具备的权限）。",
          error.requestId,
        );
      default:
        return withRequestId(error.message, error.requestId);
    }
  }
  return error instanceof Error ? error.message : "未知错误";
}

export function UserListScreen() {
  const { me, refresh, logout, logoutError } = useAuth();
  const [users, setUsers] = useState<AdminUser[] | null>(null);
  /**
   * `null` 表示「尚未加载成功」，与 `[]`（确实是空目录）区分开：
   * 一次网络故障不能伪装成「暂无可分配角色」，否则角色分配会被静默阻断。
   */
  const [roles, setRoles] = useState<RoleSummary[] | null>(null);
  const [rolesError, setRolesError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [username, setUsername] = useState("");
  const [email, setEmail] = useState("");
  const [displayName, setDisplayName] = useState("");

  const canManageUsers = me?.permissions.includes("user.manage") ?? false;
  const canManageRoles = me?.permissions.includes("role.manage") ?? false;
  const canOwnership = me?.permissions.includes("ownership.manage") ?? false;
  const canAdminister = canManageUsers || canManageRoles;

  const load = useCallback(async () => {
    setError(null);
    try {
      setUsers(await api.listUsers(USER_PAGE_LIMIT));
    } catch (e) {
      setUsers([]);
      setError(messageOf(e));
    }
  }, []);

  /** 角色目录单独加载：失败要可见、可重试，不能退化成空列表。 */
  const loadRoles = useCallback(async () => {
    setRolesError(null);
    setRoles(null);
    try {
      setRoles(await api.listRoles());
    } catch (e) {
      setRolesError(messageOf(e));
    }
  }, []);

  useEffect(() => {
    if (!canAdminister) return;
    void load();
    if (canManageRoles) void loadRoles();
  }, [canAdminister, canManageRoles, load, loadRoles]);

  async function createUser(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      const created = await api.createUser({
        username: username.trim(),
        email: email.trim() === "" ? undefined : email.trim(),
        display_name: displayName.trim() === "" ? undefined : displayName.trim(),
      });
      setUsername("");
      setEmail("");
      setDisplayName("");
      setNotice(`已创建账号 ${created.username}；请为其分配角色并绑定登录方式。`);
      await load();
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function mutateRole(
    user: AdminUser,
    role: string,
    action: "assign" | "remove",
  ): Promise<void> {
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      if (action === "assign") {
        await api.assignRole(user.username, role);
      } else {
        await api.removeRole(user.username, role);
      }
      setNotice(
        action === "assign"
          ? `已为 ${user.username} 分配角色 ${role}。`
          : `已移除 ${user.username} 的角色 ${role}。`,
      );
      // 目标是自己时角色变更已递增 users.version，本人会话立即失效；
      // 主动刷新 `/me` 让界面进入登录态，而不是继续显示已失效的会话。
      if (user.id === me?.user_id) {
        await refresh();
        return;
      }
      await load();
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  if (!canAdminister) {
    return (
      <div className="screen">
        <header className="topbar">
          <h1>用户与角色</h1>
          <div className="topbar-actions">
            <button type="button" className="button ghost" onClick={() => navigate(paths.list)}>
              我的文章
            </button>
            <button type="button" className="button ghost" onClick={() => void logout()}>
              退出
            </button>
          </div>
        </header>
        <p className="warning">
          当前账号没有 user.manage 或 role.manage 权限，无法查看或管理账号。
        </p>
      </div>
    );
  }

  // 最后一个「可登录」Owner 的判定来自后端的**全局**结果（`is_last_loginable_owner`），
  // 不看当前页：另一个可登录 Owner 落在后续页时，按页推断会把它误判成最后 Owner。
  // 界面据此提前禁用移除，后端执行时仍会在排他锁下复核（前端不是安全边界）。
  return (
    <div className="screen">
      <header className="topbar">
        <h1>用户与角色</h1>
        <div className="topbar-actions">
          <button type="button" className="button ghost" onClick={() => navigate(paths.list)}>
            我的文章
          </button>
          {me?.permissions.includes("page.read") === true && (
            <button type="button" className="button ghost" onClick={() => navigate(paths.pages)}>
              独立页面
            </button>
          )}
          {canManageRoles && (
            <button type="button" className="button ghost" onClick={() => navigate(paths.roles)}>
              角色目录
            </button>
          )}
          <button type="button" className="button ghost" onClick={() => void logout()}>
            退出
          </button>
        </div>
      </header>

      {canManageUsers && (
        <form className="editor" onSubmit={(event) => void createUser(event)}>
          <h2>创建账号</h2>
          <label>
            用户名
            <input
              value={username}
              onChange={(event) => setUsername(event.target.value)}
              autoComplete="off"
            />
          </label>
          <label>
            邮箱（可选）
            <input value={email} onChange={(event) => setEmail(event.target.value)} />
          </label>
          <label>
            展示名（可选）
            <input value={displayName} onChange={(event) => setDisplayName(event.target.value)} />
          </label>
          <div className="editor-actions">
            <button type="submit" className="button" disabled={busy || username.trim() === ""}>
              创建账号
            </button>
          </div>
        </form>
      )}

      {error !== null && <p className="error">{error}</p>}
      {notice !== null && <p className="notice">{notice}</p>}
      {logoutError !== null && <p className="error">{logoutError}</p>}
      {canManageRoles && rolesError !== null && (
        <p className="error">
          角色目录加载失败：{rolesError}
          <button type="button" className="link" onClick={() => void loadRoles()}>
            重试
          </button>
        </p>
      )}
      {users === null && <p className="muted">正在加载…</p>}
      {users !== null && users.length === 0 && error === null && (
        <p className="muted">还没有账号。</p>
      )}
      {users !== null && users.length >= USER_PAGE_LIMIT && (
        <p className="muted">
          仅显示前 {USER_PAGE_LIMIT} 个账号；更多账号请用受控 CLI 管理。
        </p>
      )}

      {users !== null && users.length > 0 && (
        <table className="posts users">
          <thead>
            <tr>
              <th>用户名</th>
              <th>展示名</th>
              <th>邮箱</th>
              <th>角色</th>
              <th>登录方式</th>
              {canManageRoles && <th>分配角色</th>}
            </tr>
          </thead>
          <tbody>
            {users.map((user) => {
              const assignable = (roles ?? []).filter(
                (role) =>
                  !user.roles.includes(role.slug) &&
                  // 授予 Owner 需要专门的所有权权限；没有就不展示该选项。
                  (role.slug !== "owner" || canOwnership),
              );
              return (
                <tr key={user.id}>
                  <td>
                    <code>{user.username}</code>
                    {user.id === me?.user_id && <span className="badge"> 我</span>}
                  </td>
                  <td>{user.display_name ?? "（未设置）"}</td>
                  <td className="muted">{user.email ?? "—"}</td>
                  <td>
                    {user.roles.length === 0 && <span className="muted">（无）</span>}
                    {user.roles.map((role) => {
                      const lastOwner = role === "owner" && user.is_last_loginable_owner;
                      return (
                        <span key={role} className="badge">
                          {role}
                          {lastOwner && <span title="最后一个可登录的 Owner">（最后 Owner）</span>}
                          {canManageRoles && (
                            <button
                              type="button"
                              className="link"
                              disabled={busy || lastOwner}
                              title={
                                lastOwner
                                  ? "这是最后一个可登录的 Owner，不能移除其 Owner 角色"
                                  : undefined
                              }
                              aria-label={`移除 ${user.username} 的角色 ${role}`}
                              onClick={() => void mutateRole(user, role, "remove")}
                            >
                              移除
                            </button>
                          )}
                        </span>
                      );
                    })}
                  </td>
                  <td className="muted">
                    {user.deleted
                      ? "已停用"
                      : user.can_login
                        ? user.password_enabled && user.external_identities > 0
                          ? "密码 + 外部身份"
                          : user.password_enabled
                            ? "本地密码"
                            : "外部身份"
                        : "无（无法登录）"}
                  </td>
                  {canManageRoles && (
                    <td>
                      {rolesError !== null ? (
                        <span className="muted">角色目录不可用</span>
                      ) : roles === null ? (
                        <span className="muted">正在加载角色…</span>
                      ) : assignable.length === 0 ? (
                        <span className="muted">暂无可分配角色</span>
                      ) : (
                        <RoleAssigner
                          username={user.username}
                          roles={assignable}
                          busy={busy}
                          onAssign={(role) => void mutateRole(user, role, "assign")}
                        />
                      )}
                    </td>
                  )}
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </div>
  );
}

/** 单个用户的角色分配控件：选择 + 提交，避免受控状态散落在列表里。 */
function RoleAssigner({
  username,
  roles,
  busy,
  onAssign,
}: {
  username: string;
  roles: RoleSummary[];
  busy: boolean;
  onAssign: (role: string) => void;
}) {
  const [selected, setSelected] = useState("");
  return (
    <>
      <select
        aria-label={`为 ${username} 选择角色`}
        value={selected}
        onChange={(event) => setSelected(event.target.value)}
      >
        <option value="">选择角色…</option>
        {roles.map((role) => (
          <option key={role.slug} value={role.slug}>
            {role.name}（{role.slug}）
          </option>
        ))}
      </select>
      <button
        type="button"
        className="button"
        disabled={busy || selected === ""}
        aria-label={`为 ${username} 添加角色`}
        onClick={() => {
          if (selected !== "") onAssign(selected);
          setSelected("");
        }}
      >
        添加角色
      </button>
    </>
  );
}
