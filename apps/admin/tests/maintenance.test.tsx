// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { maintenanceApi } from "../src/api/maintenance";
import { ApiError } from "../src/api/client";
import { AdminProviders } from "../src/providers";
import { HtmlRebuildPanel } from "../src/screens/settings/HtmlRebuildPanel";
import { counts, job, view } from "./maintenanceFixtures";
import type { HtmlRebuildJob } from "../src/api/generated";
import { queryKeys } from "../src/queryClient";

vi.mock("../src/api/maintenance", () => ({ maintenanceApi: { get: vi.fn(), start: vi.fn() } }));

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(maintenanceApi.get).mockResolvedValue(view());
  vi.mocked(maintenanceApi.start).mockResolvedValue(job());
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

function QueryProbe({ capture }: { capture: (client: QueryClient) => void }) {
  capture(useQueryClient());
  return null;
}
function mount(active = true, capture?: (client: QueryClient) => void) {
  return render(<AdminProviders>{capture && <QueryProbe capture={capture} />}<HtmlRebuildPanel active={active} /></AdminProviders>);
}
async function tick(ms = 20) { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); }

describe("内容维护面板", () => {
  it("默认只读展示三类待重建数量，连续启动点击仅发送一次POST", async () => {
    let finish!: (value: HtmlRebuildJob) => void;
    vi.mocked(maintenanceApi.start).mockReturnValue(new Promise(resolve => { finish = resolve; }));
    mount();
    const table = await screen.findByRole("table");
    expect(within(table).getByText("文章")).toBeTruthy();
    expect(within(table).getByText("独立页面")).toBeTruthy();
    expect(within(table).getByText("评论")).toBeTruthy();
    expect(within(table).getByText("5")).toBeTruthy();
    expect(maintenanceApi.start).not.toHaveBeenCalled();
    const start = screen.getByRole("button", { name: "开始重建" });
    fireEvent.click(start); fireEvent.click(start);
    await waitFor(() => expect(maintenanceApi.start).toHaveBeenCalledTimes(1));
    await act(async () => finish(job()));
    await screen.findByRole("button", { name: "正在重建" });
    expect(screen.getByRole("button", { name: "正在重建" }).hasAttribute("disabled")).toBe(true);
    expect(within(table).queryByText("5")).toBeNull();
    expect(screen.getByRole("row", { name: "文章 — 0 0" })).toBeTruthy();
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(screen.getByText(/离开或刷新此页不会中止/)).toBeTruthy();
  });

  it("2秒读取进度，展示本轮成功和跳过数量，完成后停止轮询并允许继续", async () => {
    vi.useFakeTimers();
    vi.mocked(maintenanceApi.get)
      .mockResolvedValueOnce(view({ job: job() }))
      .mockResolvedValueOnce(view({ job: job("running", { rebuilt: counts(2, 1), skipped: counts(1), batches: 1 }) }))
      .mockResolvedValue(view({ job: job("completed", { rebuilt: counts(3, 2, 1), skipped: counts(1), batches: 2, has_more: true }) }));
    let client!: QueryClient;
    mount(true, current => { client = current; }); await tick();
    client.setQueryData(queryKeys.comments(1, "all"), { items: [{ content_html: "旧评论HTML" }] });
    expect(maintenanceApi.start).not.toHaveBeenCalled();
    await tick(2_000);
    expect(maintenanceApi.get).toHaveBeenCalledTimes(2);
    const row = screen.getByRole("row", { name: "文章 — 2 1" });
    expect(within(row).getByText("2")).toBeTruthy();
    await tick(2_000);
    expect(screen.getByRole("button", { name: "继续执行" }).hasAttribute("disabled")).toBe(false);
    expect(client.getQueryState(queryKeys.comments(1, "all"))?.isInvalidated).toBe(true);
    await tick(8_000);
    expect(maintenanceApi.get).toHaveBeenCalledTimes(3);
  });

  it("重新进入面板恢复服务端运行任务，切走tab停止请求，返回时读取最新", async () => {
    vi.useFakeTimers();
    vi.mocked(maintenanceApi.get).mockResolvedValue(view({ job: job() }));
    const mounted = mount(false); await tick();
    expect(maintenanceApi.get).not.toHaveBeenCalled();
    mounted.rerender(<AdminProviders><HtmlRebuildPanel active /></AdminProviders>); await tick();
    expect(maintenanceApi.get).toHaveBeenCalledTimes(1);
    expect(maintenanceApi.start).not.toHaveBeenCalled();
    mounted.rerender(<AdminProviders><HtmlRebuildPanel active={false} /></AdminProviders>); await tick(5_000);
    expect(maintenanceApi.get).toHaveBeenCalledTimes(1);
    vi.mocked(maintenanceApi.get).mockResolvedValue(view({ pending: counts(), job: job("completed", { pending: counts(), has_more: false }) }));
    mounted.rerender(<AdminProviders><HtmlRebuildPanel active /></AdminProviders>); await tick();
    expect(maintenanceApi.get).toHaveBeenCalledTimes(2);
    expect(screen.getByText("内容重建完成。")).toBeTruthy();
  });

  it("启动期间先前的只读请求迟到不会把运行中的任务改回空闲", async () => {
    let finishRead!: (value: ReturnType<typeof view>) => void;
    mount(); await screen.findByRole("table");
    vi.mocked(maintenanceApi.get).mockReturnValueOnce(new Promise(resolve => { finishRead = resolve; }));
    fireEvent.click(screen.getByRole("button", { name: "刷新进度" }));
    await waitFor(() => expect(maintenanceApi.get).toHaveBeenCalledTimes(2));
    fireEvent.click(screen.getByRole("button", { name: "开始重建" }));
    await screen.findByRole("button", { name: "正在重建" });
    await act(async () => finishRead(view()));
    expect(screen.getByRole("button", { name: "正在重建" }).hasAttribute("disabled")).toBe(true);
    expect(maintenanceApi.start).toHaveBeenCalledTimes(1);
  });

  it("服务端中断任务不自动创建新任务，停止轮询并提供继续入口", async () => {
    vi.useFakeTimers();
    vi.mocked(maintenanceApi.get).mockResolvedValue(view({ job: job("interrupted") }));
    mount(); await tick(); await tick(8_000);
    expect(maintenanceApi.get).toHaveBeenCalledTimes(1);
    expect(maintenanceApi.start).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "继续执行" }).hasAttribute("disabled")).toBe(false);
    expect(screen.getByText(/上一次重建已中断/)).toBeTruthy();
  });

  it("部分失败保留成功数量，显示类型与编号并安全重试，不回显内部消息", async () => {
    const failureId = "7476c0c3-90fc-4cbe-bf92-4474e76214fc";
    vi.mocked(maintenanceApi.get).mockResolvedValue(view({ job: job("failed", {
      rebuilt: counts(4), failure: { kind: "page", id: failureId, message: "postgres://private/internal SQL" },
    }) }));
    mount();
    await screen.findByText(/失败内容：独立页面/);
    expect(screen.getByText(new RegExp(failureId))).toBeTruthy();
    expect(screen.queryByText(/private\/internal SQL/)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "再次执行" }));
    await waitFor(() => expect(maintenanceApi.start).toHaveBeenCalledTimes(1));
  });

  it.each([
    ["没有待重建内容", view({ pending: counts() })],
    ["恢复隔离", view({ available: false, pending: null })],
    ["未知待重建数量", view({ pending: null })],
  ])("%s时不能启动", async (_label, result) => {
    vi.mocked(maintenanceApi.get).mockResolvedValue(result);
    mount(); await screen.findByRole("table");
    expect(screen.getByRole("button", { name: "开始重建" }).hasAttribute("disabled")).toBe(true);
    expect(maintenanceApi.start).not.toHaveBeenCalled();
  });

  it.each([new ApiError(403, "需要站点设置权限", "forbidden"), new TypeError("网络连接失败")])("轮询失败有反馈，停止自动请求，手动重试可恢复", async cause => {
    vi.useFakeTimers();
    vi.mocked(maintenanceApi.get)
      .mockResolvedValueOnce(view({ job: job() }))
      .mockRejectedValueOnce(cause)
      .mockResolvedValueOnce(view({ job: job() }))
      .mockResolvedValue(view({ pending: counts(), job: job("completed", { pending: counts(), has_more: false }) }));
    mount(); await tick(); await tick(2_000);
    expect(screen.getByText(/进度更新已暂停/)).toBeTruthy();
    if (cause instanceof ApiError) expect(screen.getByText(/没有权限：需要站点设置权限/)).toBeTruthy();
    await tick(8_000);
    expect(maintenanceApi.get).toHaveBeenCalledTimes(2);
    fireEvent.click(screen.getByRole("button", { name: "刷新进度" })); await tick();
    expect(maintenanceApi.get).toHaveBeenCalledTimes(3);
    expect(screen.queryByText(/进度更新已暂停/)).toBeNull();
    await tick(2_000);
    expect(maintenanceApi.get).toHaveBeenCalledTimes(4);
    expect(screen.getByText("内容重建完成。")).toBeTruthy();
  });

  it("启动403显示权限反馈并提供刷新入口，不自动重复写入", async () => {
    vi.mocked(maintenanceApi.start).mockRejectedValue(new ApiError(403, "需要站点设置权限", "forbidden"));
    mount(); await screen.findByRole("table");
    fireEvent.click(screen.getByRole("button", { name: "开始重建" }));
    await screen.findByText(/没有权限：需要站点设置权限/);
    expect(maintenanceApi.start).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "刷新进度" }).hasAttribute("disabled")).toBe(false);
  });
});
