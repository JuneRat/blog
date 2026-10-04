import { App as AntdApp } from "antd";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { ReactNode } from "react";
import { blockHistoryNavigation } from "./router";

interface UnsavedValue {
  /** 当前屏幕声明的未保存提示；null 表示没有未保存改动。 */
  readMessage: (() => string | null) | null;
  setGuard: (read: (() => string | null) | null) => void;
}

const UnsavedContext = createContext<UnsavedValue | null>(null);

/**
 * 「有未保存改动」的跨层登记处。
 *
 * 屏幕知道自己脏不脏，外壳才知道用户要去哪；两者不能互相 import，
 * 所以由 Provider 做这一格：屏幕登记提示，外壳在导航前据此确认。
 */
export function UnsavedChangesProvider({ children }: { children: ReactNode }) {
  const [guard, setGuardState] = useState<{
    read: (() => string | null) | null;
  }>({ read: null });
  const setGuard = useCallback(
    (read: (() => string | null) | null) => setGuardState({ read }),
    [],
  );
  const readMessage = guard.read;
  const { modal } = AntdApp.useApp();
  useLayoutEffect(() => {
    if (readMessage === null) return;
    return blockHistoryNavigation((decide) => {
      const message = readMessage();
      if (message === null) {
        decide(true);
        return () => {};
      }
      const dialog = modal.confirm({
        title: "有未保存的修改",
        content: message,
        okText: "放弃修改并离开",
        cancelText: "留在此页",
        okButtonProps: { danger: true },
        onOk: () => decide(true),
        onCancel: () => decide(false),
      });
      return () => dialog.destroy();
    });
  }, [readMessage, modal]);
  const value = useMemo(
    () => ({ readMessage, setGuard }),
    [readMessage, setGuard],
  );
  return (
    <UnsavedContext.Provider value={value}>{children}</UnsavedContext.Provider>
  );
}

/** 外壳读取当前登记。没有 Provider 时退化为「无未保存改动」，而不是抛错。 */
const NO_GUARD: UnsavedValue = { readMessage: null, setGuard: () => undefined };
export function useUnsavedChanges(): UnsavedValue {
  return useContext(UnsavedContext) ?? NO_GUARD;
}

/**
 * 统一的「离开前确认」：有未保存登记就先确认，否则直接执行 `action`。
 *
 * **所有用户主动离开的入口都要用它**——侧栏导航、退出登录、屏内的「去标签目录」
 * 这类链接；只接在菜单上，别处一点就走，保护等于没有。
 *
 * 反面同样重要：程序自身的跳转**不要**用它。保存成功后 `replace` 到新 slug、
 * 删除成功后回列表，那些时刻内容已经落盘或本来就该丢弃，多一次确认是打扰。
 */
export function useLeaveConfirmation(): (
  action: () => void,
  okText?: string,
) => void {
  const { modal } = AntdApp.useApp();
  const { readMessage } = useUnsavedChanges();
  return useCallback(
    (action: () => void, okText = "放弃修改并离开") => {
      const message = readMessage?.() ?? null;
      if (message === null) {
        action();
        return;
      }
      modal.confirm({
        title: "有未保存的修改",
        content: message,
        okText,
        cancelText: "留在此页",
        okButtonProps: { danger: true },
        onOk: action,
      });
    },
    [readMessage, modal],
  );
}

/**
 * 屏幕登记「有未保存改动」，并覆盖浏览器级的离开路径。
 *
 * - 侧边栏与屏内导航：由 `useLeaveConfirmation` 确认；
 * - 刷新/关闭标签页：这里的 `beforeunload`；
 * - 后台历史前进/后退：Provider 注册统一拦截，确认前不发布新路由，保留编辑组件；
 * - 跨文档离开后台：同样由 beforeunload 请求浏览器原生确认。
 */
/** A getter protects immediate navigation before a batched form subscription renders. */
export function useUnsavedGuard(
  dirty: boolean | (() => boolean),
  message: string,
): void {
  const { setGuard } = useUnsavedChanges();
  const current = useRef(dirty);
  current.current = dirty;
  const readMessage = useCallback(() => {
    const value = current.current;
    return (typeof value === "function" ? value() : value) ? message : null;
  }, [message]);
  useLayoutEffect(() => {
    setGuard(readMessage);
    return () => setGuard(null);
  }, [readMessage, setGuard]);
  useEffect(() => {
    const onBeforeUnload = (event: BeforeUnloadEvent): void => {
      const pending = readMessage();
      if (pending === null) return;
      event.preventDefault();
      event.returnValue = pending;
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    return () => window.removeEventListener("beforeunload", onBeforeUnload);
  }, [readMessage]);
}
