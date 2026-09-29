import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { ReactNode } from "react";
import { ApiError, api, loginUrl, setCsrfToken, setUnauthorizedHandler } from "./api";
import { messageOf } from "./apiError";
import type { Me, ProviderSummary } from "./types";
import { TimeZoneContext } from "./timeZone";

type Status = "loading" | "anonymous" | "authenticated";

interface AuthValue {
  status: Status;
  me: Me | null;
  providers: ProviderSummary[];
  providersLoaded: boolean;
  /** 最近一次退出失败的提示；失败时会话仍然有效，不能假装已退出。 */
  logoutError: string | null;
  refresh: () => Promise<void>;
  updateTimeZone: (timeZone: string) => void;
  logout: () => Promise<void>;
  goToLogin: () => Promise<void>;
}

const AuthContext = createContext<AuthValue | null>(null);

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

  const clearSession = useCallback(() => {
    setCsrfToken(null);
    setMe(null);
    statusRef.current = "anonymous";
    setStatus("anonymous");
  }, []);

  const refresh = useCallback(async () => {
    try {
      const current = await api.me();
      setCsrfToken(current.csrf_token);
      setMe(current);
      statusRef.current = "authenticated";
      setStatus("authenticated");
    } catch (error) {
      // Another refresh or an expired session superseded this request.
      if (error instanceof DOMException && error.name === "AbortError") return;
      if (error instanceof ApiError && error.status === 401) {
        clearSession();
        return;
      }
      throw error;
    }
  }, [clearSession]);

  const updateTimeZone = useCallback((timeZone: string) => {
    setMe((current) => current === null ? null : { ...current, time_zone: timeZone });
  }, []);

  const goToLogin = useCallback(async () => {
    clearSession();
    const url = await loginUrl("/admin/");
    if (url !== null && statusRef.current === "anonymous") {
      window.location.assign(url);
    }
  }, [clearSession]);

  useEffect(() => {
    setUnauthorizedHandler(() => {
      if (suppressUnauthorizedRef.current) return;
      if (statusRef.current === "authenticated") {
        void goToLogin();
      } else {
        clearSession();
      }
    });
    return () => setUnauthorizedHandler(null);
  }, [goToLogin, clearSession]);

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
        if (!cancelled) clearSession();
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [refresh, clearSession]);

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
    clearSession();
    window.location.assign("/admin/");
  }, [clearSession]);

  const value = useMemo<AuthValue>(
    () => ({
      status,
      me,
      providers,
      providersLoaded,
      logoutError,
      refresh,
      updateTimeZone,
      logout,
      goToLogin,
    }),
    [status, me, providers, providersLoaded, logoutError, refresh, updateTimeZone, logout, goToLogin],
  );

  return <AuthContext.Provider value={value}>
    <TimeZoneContext.Provider value={me?.time_zone ?? "UTC"}>{children}</TimeZoneContext.Provider>
  </AuthContext.Provider>;
}

export function useAuth(): AuthValue {
  const value = useContext(AuthContext);
  if (value === null) throw new Error("useAuth 必须在 AuthProvider 内使用");
  return value;
}
