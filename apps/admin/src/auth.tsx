import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { ApiError, api, loginUrl, setCsrfToken, setUnauthorizedHandler, withRequestId } from "./api";
import type { Me, ProviderSummary } from "./types";

type Status = "loading" | "anonymous" | "authenticated";

interface AuthValue {
  status: Status;
  me: Me | null;
  providers: ProviderSummary[];
  providersLoaded: boolean;
  /** 最近一次退出失败的提示；失败时会话仍然有效，不能假装已退出。 */
  logoutError: string | null;
  refresh: () => Promise<void>;
  logout: () => Promise<void>;
  goToLogin: () => Promise<void>;
}

const AuthContext = createContext<AuthValue | null>(null);

function messageOf(error: unknown): string {
  if (error instanceof ApiError) return withRequestId(error.message, error.requestId);
  return error instanceof Error ? error.message : "未知错误";
}

/**
 * 会话与 CSRF 生命周期：应用启动读一次 `/me`（拿到内存 CSRF token），
 * 之后任何 401 都清空 token；已登录状态下的会话过期直接跳登录，
 * 未登录状态只切到登录页（避免无绑定用户被反复弹回 IdP 形成重定向环）。
 */
export function AuthProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<Status>("loading");
  const [me, setMe] = useState<Me | null>(null);
  const [providers, setProviders] = useState<ProviderSummary[]>([]);
  const [providersLoaded, setProvidersLoaded] = useState(false);
  const [logoutError, setLogoutError] = useState<string | null>(null);
  const statusRef = useRef<Status>("loading");
  /** 退出流程内抑制 401 处理器，避免「会话已失效时退出」被重定向到登录页。 */
  const suppressUnauthorizedRef = useRef(false);

  useEffect(() => {
    statusRef.current = status;
  }, [status]);

  const refresh = useCallback(async () => {
    try {
      const current = await api.me();
      setCsrfToken(current.csrf_token);
      setMe(current);
      setStatus("authenticated");
    } catch (error) {
      if (error instanceof ApiError && error.status === 401) {
        setCsrfToken(null);
        setMe(null);
        setStatus("anonymous");
        return;
      }
      throw error;
    }
  }, []);

  const goToLogin = useCallback(async () => {
    const url = await loginUrl("/admin/");
    if (url !== null) {
      window.location.assign(url);
      return;
    }
    setStatus("anonymous");
  }, []);

  useEffect(() => {
    setUnauthorizedHandler(() => {
      if (suppressUnauthorizedRef.current) return;
      if (statusRef.current === "authenticated") {
        void goToLogin();
      } else {
        setStatus("anonymous");
      }
    });
    return () => setUnauthorizedHandler(null);
  }, [goToLogin]);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const list = await api.providers();
        if (!cancelled) {
          setProviders(list);
          setProvidersLoaded(true);
        }
      } catch {
        if (!cancelled) setProvidersLoaded(true);
      }
      try {
        await refresh();
      } catch {
        if (!cancelled) setStatus("anonymous");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [refresh]);

  const logout = useCallback(async () => {
    setLogoutError(null);
    suppressUnauthorizedRef.current = true;
    try {
      await api.logout();
    } catch (error) {
      // 401 表示会话本就失效，等价于已退出，继续本地清理；
      // 其它失败（网络、403 CSRF、5xx）会话仍然有效：绝不能假装退出——
      // 否则跳回 /admin/ 后 /me 会立刻把用户重新认回来。
      if (!(error instanceof ApiError && error.status === 401)) {
        setLogoutError(`退出失败：${messageOf(error)}`);
        return;
      }
    } finally {
      suppressUnauthorizedRef.current = false;
    }
    setCsrfToken(null);
    setMe(null);
    setStatus("anonymous");
    window.location.assign("/admin/");
  }, []);

  const value = useMemo<AuthValue>(
    () => ({
      status,
      me,
      providers,
      providersLoaded,
      logoutError,
      refresh,
      logout,
      goToLogin,
    }),
    [status, me, providers, providersLoaded, logoutError, refresh, logout, goToLogin],
  );

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth(): AuthValue {
  const value = useContext(AuthContext);
  if (value === null) throw new Error("useAuth 必须在 AuthProvider 内使用");
  return value;
}
