import { QueryClientProvider } from "@tanstack/react-query";
import { App as AntdApp, ConfigProvider, theme as antdTheme } from "antd";
import zhCN from "antd/locale/zh_CN";
import { useEffect, useState } from "react";
import { createQueryClient } from "./queryClient";
import type { ReactNode } from "react";

/**
 * 后台的设计语言集中在这里。
 *
 * 迁移前这些值散在 `styles.css` 的 `:root` 里（`--accent`/`--danger`），
 * 现在由 antd 的 Design Token 接管；自定义组件用 `theme.useToken()` 读同一套值，
 * 不再各写一份十六进制颜色。
 *
 * 只设种子 token，不逐个覆盖组件 token：先吃 antd 默认视觉，
 * 真有具体不对的地方再加 `components` 覆盖，否则以后分不清是自己调的还是组件本身的。
 */
export const ADMIN_TOKENS = {
  colorPrimary: "#1f6feb",
  colorError: "#b42318",
  colorInfo: "#1f6feb",
  borderRadius: 6,
} as const;

const DARK_QUERY = "(prefers-color-scheme: dark)";

/** jsdom 没有 matchMedia；无该 API 时按浅色处理，测试里不会因此炸掉。 */
function prefersDark(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") return false;
  return window.matchMedia(DARK_QUERY).matches;
}

/** 跟随系统深浅色：与迁移前 `color-scheme: light dark` 的表现一致（没有手动开关）。 */
export function useSystemDark(): boolean {
  const [dark, setDark] = useState(prefersDark);
  useEffect(() => {
    if (typeof window.matchMedia !== "function") return;
    const query = window.matchMedia(DARK_QUERY);
    const update = (event: MediaQueryListEvent): void => setDark(event.matches);
    query.addEventListener("change", update);
    return () => query.removeEventListener("change", update);
  }, []);
  return dark;
}

/**
 * 必须包住所有屏幕（含测试里直接渲染的入口）：
 *
 * - `locale` 决定分页、日期、确认弹窗按钮等内置文案，后台是中文站点；
 * - `algorithm` 决定深浅色。注意 `color-scheme` 只影响浏览器原生控件，
 *   管不到 antd 组件，所以深色必须显式给算法；
 * - antd 的 `App` 提供 message/modal/notification 的上下文版本。静态方法
 *   （`Modal.confirm` 等）不消费 ConfigProvider，主题和中文 locale 都不生效。
 */
export function AdminProviders({ children }: { children: ReactNode }) {
  const dark = useSystemDark();
  // 惰性创建：每次挂载一个 client（测试因此天然隔离），而不是模块级单例。
  const [queryClient] = useState(createQueryClient);
  useEffect(() => () => queryClient.clear(), [queryClient]);
  return (
    <ConfigProvider
      locale={zhCN}
      // antd 默认会在「默认类型」的两字中文按钮里插入空格（保存 → 保 存），
      // 而 text/link 按钮不插。同类按钮文案不一致，也会让按无障碍名定位的测试
      // 时灵时不灵；后台按钮密度高，统一不插空格更好排版。
      button={{ autoInsertSpace: false }}
      theme={{
        algorithm: dark ? antdTheme.darkAlgorithm : antdTheme.defaultAlgorithm,
        token: ADMIN_TOKENS,
      }}
    >
      <QueryClientProvider client={queryClient}>
        <AntdApp>{children}</AntdApp>
      </QueryClientProvider>
    </ConfigProvider>
  );
}
