import { beforeEach, vi } from "vitest";
import { configure } from "@testing-library/react";

// Lazy screens and Ant Design rendering can exceed Testing Library's default
// one-second deadline on a shared CI runner. Still wait for the actual UI state.
configure({ asyncUtilTimeout: 5_000 });

vi.mock("./components/vditorRuntime", async () => ({
  editorCdn: "/admin/assets/vditor-test",
  loadVditor: async () => (await import("../tests/fakes/vditor")).default,
}));

beforeEach(() => { localStorage.clear(); sessionStorage.clear(); });

/**
 * 测试环境补丁。
 *
 * jsdom 不实现 antd 依赖的几个浏览器 API；缺失时组件会在渲染期抛错，
 * 与业务无关的失败会掩盖真正的回归。这里只补最小可用行为，不做真实布局计算。
 */

if (typeof window.matchMedia !== "function") {
  Object.defineProperty(window, "matchMedia", {
    writable: true,
    value: (query: string): MediaQueryList =>
      ({
        matches: false,
        media: query,
        onchange: null,
        addEventListener: () => undefined,
        removeEventListener: () => undefined,
        addListener: () => undefined,
        removeListener: () => undefined,
        dispatchEvent: () => false,
      }) as unknown as MediaQueryList,
  });
}

if (typeof globalThis.ResizeObserver !== "function") {
  class ResizeObserverStub {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  }
  Object.defineProperty(globalThis, "ResizeObserver", {
    writable: true,
    value: ResizeObserverStub,
  });
}

// antd 的 message/notification 用 getComputedStyle 读取容器尺寸；jsdom 的实现
// 缺少部分声明，这里兜住调用而不是让断言失败在样式读取上。
if (typeof window.getComputedStyle !== "function") {
  Object.defineProperty(window, "getComputedStyle", {
    value: () => ({ getPropertyValue: () => "" }),
  });
}
