import { App as AntdApp } from "antd";
import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";
import { blockHistoryNavigation } from "./router";

interface UnsavedValue {
  /** 当前屏幕声明的未保存提示；null 表示没有未保存改动。 */
  message: string | null;
  setMessage: (message: string | null) => void;
}

const UnsavedContext = createContext<UnsavedValue | null>(null);

/**
 * 「有未保存改动」的跨层登记处。
 *
 * 屏幕知道自己脏不脏，外壳才知道用户要去哪；两者不能互相 import，
 * 所以由 Provider 做这一格：屏幕登记提示，外壳在导航前据此确认。
 */
export function UnsavedChangesProvider({ children }: { children: ReactNode }) {
  const [message, setMessage] = useState<string | null>(null);
  const { modal } = AntdApp.useApp();
  useLayoutEffect(() => {
    if (message === null) return;
    return blockHistoryNavigation(decide => {
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
  }, [message, modal]);
  const value = useMemo(() => ({ message, setMessage }), [message]);
  return <UnsavedContext.Provider value={value}>{children}</UnsavedContext.Provider>;
}

/** 外壳读取当前登记。没有 Provider 时退化为「无未保存改动」，而不是抛错。 */
export function useUnsavedChanges(): UnsavedValue {
  return useContext(UnsavedContext) ?? { message: null, setMessage: () => undefined };
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
export function useLeaveConfirmation(): (action: () => void, okText?: string) => void {
  const { modal } = AntdApp.useApp();
  const { message } = useUnsavedChanges();
  return useCallback(
    (action: () => void, okText = "放弃修改并离开") => {
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
    [message, modal],
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
export function useUnsavedGuard(dirty: boolean, message: string): void {
  const { setMessage } = useUnsavedChanges();

  useLayoutEffect(() => {
    if (!dirty) return;
    setMessage(message);
    return () => setMessage(null);
  }, [dirty, message, setMessage]);

  useEffect(() => {
    if (!dirty) return;
    const onBeforeUnload = (event: BeforeUnloadEvent): void => {
      // 现代浏览器只需要 preventDefault；returnValue 仍被部分实现要求。
      event.preventDefault();
      event.returnValue = message;
    };
    window.addEventListener("beforeunload", onBeforeUnload);
    return () => window.removeEventListener("beforeunload", onBeforeUnload);
  }, [dirty, message]);
}
