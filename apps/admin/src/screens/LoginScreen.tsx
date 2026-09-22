import { useRef, useState } from "react";
import type { FormEvent } from "react";
import { ApiError, api, withRequestId } from "../api";
import { useAuth } from "../auth";

function messageOf(error: unknown): string {
  if (error instanceof ApiError) return withRequestId(error.message, error.requestId);
  return error instanceof Error ? error.message : "未知错误";
}

/**
 * 登录页：本土密码 + 已绑定的外部身份。
 *
 * 密码登录由后端强制（Argon2id 校验 + 失败限流），前端不缓存任何凭据：
 * 提交后只等 cookie 与 `/me`。失败时清空密码框，避免留在屏幕或表单状态里。
 * 提供商列表来自公开只读 `/auth/providers`（id/展示名/类型）。
 */
export function LoginScreen() {
  const { providers, providersLoaded, refresh } = useAuth();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** 提交中的同步闸门：按钮 disabled 要等下一次渲染，双击仍可能进两次。 */
  const inFlight = useRef(false);

  const canSubmit = username.trim().length > 0 && password.length > 0 && !submitting;

  async function onSubmit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (!canSubmit || inFlight.current) return;
    inFlight.current = true;
    setSubmitting(true);
    setError(null);
    try {
      await api.loginWithPassword({
        username: username.trim(),
        password,
        next: "/admin/",
      });
      // 成功后立即从内存状态抹掉口令，再刷新会话。
      setPassword("");
      // 登录成功后重新读取会话与内存 CSRF token，路由守卫随即进入后台。
      await refresh();
    } catch (cause) {
      setError(messageOf(cause));
      setPassword("");
    } finally {
      inFlight.current = false;
      setSubmitting(false);
    }
  }

  return (
    <div className="screen login">
      <h1>博客后台</h1>
      <p className="muted">使用本站账号密码，或已绑定的外部身份登录。</p>

      <form className="login-form" onSubmit={onSubmit}>
        <label>
          用户名
          <input
            name="username"
            autoComplete="username"
            value={username}
            onChange={(event) => setUsername(event.target.value)}
          />
        </label>
        <label>
          密码
          <input
            name="password"
            type="password"
            autoComplete="current-password"
            value={password}
            onChange={(event) => setPassword(event.target.value)}
          />
        </label>
        {error !== null && <p className="error">{error}</p>}
        <button className="button" type="submit" disabled={!canSubmit}>
          {submitting ? "正在登录…" : "登录"}
        </button>
        <p className="muted">
          连续失败会临时锁定账号；密码由运维用 <code>blog user passwd</code> 设置或重置。
        </p>
      </form>

      {providersLoaded && providers.length > 0 && (
        <>
          <p className="muted">或使用已绑定的外部身份：</p>
          <div className="provider-list">
            {providers.map((provider) => (
              <a
                key={provider.id}
                className="button ghost"
                href={`/auth/login?provider=${encodeURIComponent(provider.id)}&next=${encodeURIComponent("/admin/")}`}
              >
                使用 {provider.name} 登录
              </a>
            ))}
          </div>
        </>
      )}
    </div>
  );
}
