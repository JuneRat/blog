import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TimeZoneContext } from "../timeZone";
import { ContentLifecycleControls } from "./ContentLifecycleControls";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("内容状态操作", () => {
  it("只允许未来预约，并按站点时区提交 UTC 时间", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-09-27T00:00:00Z"));
    const onAction = vi.fn().mockResolvedValue(undefined);
    render(<TimeZoneContext.Provider value="Asia/Shanghai"><ContentLifecycleControls status="draft" publishedAt={null} disabled={false}
      canPublish canUnpublish canArchive onAction={onAction} /></TimeZoneContext.Provider>);
    const button = screen.getByRole("button", { name: "预约发布" });
    expect((button as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("预约发布时间（Asia/Shanghai）"), { target: { value: "2026-09-28T12:30" } });
    fireEvent.click(button);
    expect(onAction).toHaveBeenCalledWith("schedule", "2026-09-28T04:30:00.000Z");
  });

  it("预约回显和时区切换使用服务器时刻，拒绝夏令时歧义输入", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-01T00:00:00Z"));
    const onAction = vi.fn().mockResolvedValue(undefined);
    const controls = <ContentLifecycleControls status="scheduled" publishedAt="2026-09-28T17:30:00Z"
      disabled={false} canPublish canUnpublish canArchive onAction={onAction} />;
    const { rerender } = render(<TimeZoneContext.Provider value="Asia/Shanghai">{controls}</TimeZoneContext.Provider>);
    expect((screen.getByLabelText("预约发布时间（Asia/Shanghai）") as HTMLInputElement).value).toBe("2026-09-29T01:30");
    rerender(<TimeZoneContext.Provider value="America/New_York">{controls}</TimeZoneContext.Provider>);
    const input = screen.getByLabelText("预约发布时间（America/New_York）");
    expect((input as HTMLInputElement).value).toBe("2026-09-28T13:30");
    fireEvent.change(input, { target: { value: "2026-11-01T01:30" } });
    expect(screen.getByRole("alert").textContent).toContain("重复/不存在");
    const button = screen.getByRole("button", { name: "更新预约" });
    expect((button as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(button);
    expect(onAction).not.toHaveBeenCalled();
  });

  it("归档只提供退回草稿，操作权限分别检查", () => {
    const onAction = vi.fn().mockResolvedValue(undefined);
    render(<ContentLifecycleControls status="archived" publishedAt={null} disabled={false}
      canPublish={false} canUnpublish canArchive={false} onAction={onAction} />);
    expect(screen.queryByRole("button", { name: "发布" })).toBeNull();
    expect(screen.queryByRole("button", { name: "预约发布" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "退回草稿" }));
    expect(onAction).toHaveBeenCalledWith("unpublish");
  });
});
