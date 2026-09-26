import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ContentLifecycleControls } from "./ContentLifecycleControls";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("内容状态操作", () => {
  it("只允许未来预约，并将本地输入转换为带时区的时间", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-09-27T00:00:00Z"));
    const onAction = vi.fn().mockResolvedValue(undefined);
    render(<ContentLifecycleControls status="draft" publishedAt={null} disabled={false}
      canPublish canUnpublish canArchive onAction={onAction} />);
    const button = screen.getByRole("button", { name: "预约发布" });
    expect((button as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("预约发布时间（本地时间）"), { target: { value: "2026-09-28T12:30" } });
    fireEvent.click(button);
    expect(onAction).toHaveBeenCalledWith("schedule", new Date("2026-09-28T12:30").toISOString());
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
