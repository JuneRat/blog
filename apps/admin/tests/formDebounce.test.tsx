import { act, cleanup, renderHook } from "@testing-library/react";
import useEsmDebounce from "antd/es/form/hooks/useDebounce";
import useCommonJsDebounce from "antd/lib/form/hooks/useDebounce";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

beforeEach(() => vi.useFakeTimers());

afterEach(() => {
  cleanup();
  vi.runOnlyPendingTimers();
  vi.useRealTimers();
});

describe.each([
  ["ESM", useEsmDebounce],
  ["CommonJS", useCommonJsDebounce],
] as const)("表单反馈的延迟更新（%s）", (_bundle, useDebounce) => {
  it("卸载表单时取消尚未执行的更新", () => {
    const { unmount } = renderHook(() => useDebounce<string>([]));
    expect(vi.getTimerCount()).toBe(1);

    unmount();

    // 遗留回调会在 jsdom 销毁后访问 window，使已通过的测试仍以失败退出。
    expect(vi.getTimerCount()).toBe(0);
  });

  it("保留延迟清空反馈及最新值覆盖旧值的行为", () => {
    const { result, rerender } = renderHook(({ errors }) => useDebounce(errors), {
      initialProps: { errors: ["旧错误"] },
    });
    act(() => vi.advanceTimersByTime(0));
    rerender({ errors: [] });
    act(() => vi.advanceTimersByTime(9));
    expect(result.current).toEqual(["旧错误"]);

    rerender({ errors: ["新错误"] });
    act(() => vi.advanceTimersByTime(10));
    expect(result.current).toEqual(["新错误"]);

    rerender({ errors: [] });
    act(() => vi.advanceTimersByTime(10));
    expect(result.current).toEqual([]);
  });
});
