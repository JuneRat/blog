// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { tasksApi } from "../src/api/tasks";
import { ApiError } from "../src/api/client";
import { App } from "../src/App";
import { AdminProviders } from "../src/providers";
import { TaskManagementScreen } from "../src/screens/TaskManagementScreen";
import { TimeZoneContext } from "../src/timeZoneContext";
import { paths } from "../src/router";
import { report, counts } from "./maintenanceFixtures";
import { schedule, task, taskView } from "./taskFixtures";
import type { TaskRun } from "../src/api/generated";
import { queryKeys } from "../src/queryClient";

const auth = vi.hoisted(() => ({ permissions: ["settings.manage"] }));
vi.mock("../src/auth", () => ({ useAuth: () => ({ status: "authenticated", me: { user_id: "task-owner", permissions: auth.permissions } }) }));
vi.mock("../src/api/tasks", () => ({ tasksApi: { get: vi.fn(), start: vi.fn(), retry: vi.fn(), cancel: vi.fn(), saveRetentionSchedule: vi.fn() } }));
beforeEach(() => {
  vi.resetAllMocks(); auth.permissions = ["settings.manage"];
  window.history.replaceState(null, "", paths.tasks);
  vi.mocked(tasksApi.get).mockResolvedValue(taskView());
});
afterEach(() => { cleanup(); vi.useRealTimers(); });
function QueryProbe({ capture }: { capture: (client: QueryClient) => void }) { capture(useQueryClient()); return null; }
function mount(timeZone = "UTC", capture?: (client: QueryClient) => void) {
  return render(<AdminProviders>{capture && <QueryProbe capture={capture} />}<TimeZoneContext.Provider value={timeZone}><TaskManagementScreen /></TimeZoneContext.Provider></AdminProviders>);
}
function clock() { vi.useFakeTimers(); vi.setSystemTime(new Date("2026-10-01T00:00:00Z")); }
async function tick(ms = 20) { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); }
function beforeUnloadBlocked() {
  const event = new Event("beforeunload", { cancelable: true }); window.dispatchEvent(event); return event.defaultPrevented;
}

describe("独立任务管理", () => {
  it("独立路由和菜单显示三组任务功能，不自动创建任务", async () => {
    render(<App />);
    await screen.findByRole("tab", { name: "内容重建" });
    expect(screen.getByRole("menuitem", { name: "任务管理" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "任务管理" })).toBeTruthy();
    expect(screen.getAllByRole("tab")).toHaveLength(3);
    expect(tasksApi.start).not.toHaveBeenCalled();
    expect(tasksApi.get).toHaveBeenCalledWith({ kind: undefined, cursor: undefined, limit: 20 }, expect.any(AbortSignal));
  });

  it("立即执行同步防重复点击，排队任务无离开保护并可从服务端恢复", async () => {
    let finish!: (run: TaskRun) => void;
    vi.mocked(tasksApi.start).mockReturnValue(new Promise(resolve => { finish = resolve; }));
    mount(); const button = await screen.findByRole("button", { name: "开始重建" });
    fireEvent.click(button); fireEvent.click(button);
    await waitFor(() => expect(tasksApi.start).toHaveBeenCalledTimes(1));
    expect(tasksApi.start).toHaveBeenCalledWith({ kind: "html_rebuild", run_at: null });
    vi.mocked(tasksApi.get).mockResolvedValue(taskView({ latest: [task()] }));
    await act(async () => finish(task()));
    await screen.findByRole("button", { name: "等待执行" });
    expect(screen.getByRole("button", { name: "等待执行" }).hasAttribute("disabled")).toBe(true);
    expect(beforeUnloadBlocked()).toBe(false);
    expect(screen.queryByRole("progressbar")).toBeNull();
  });

  it("提交响应中的运行任务立即清空待重建快照，不等待下次GET", async () => {
    vi.mocked(tasksApi.get).mockResolvedValueOnce(taskView()).mockImplementation(() => new Promise(() => {}));
    vi.mocked(tasksApi.start).mockResolvedValue(task({ status: "running", can_cancel: false,
      report: { html: report({ pending: null }), retention: null, publication: null, error: null } }));
    let client!: QueryClient;
    mount("UTC", current => { client = current; });
    fireEvent.click(await screen.findByRole("button", { name: "开始重建" }));
    await screen.findByRole("button", { name: "正在重建" });
    expect(client.getQueryData(queryKeys.tasks())).toMatchObject({ pending_html: null });
  });

  it("一次性重建按站点时区提交UTC时刻", async () => {
    clock(); mount("Asia/Shanghai"); await tick();
    fireEvent.click(screen.getByLabelText("一次性计划"));
    fireEvent.change(screen.getByLabelText("重建执行时间（Asia/Shanghai）"), { target: { value: "2026-10-02T12:30" } });
    vi.mocked(tasksApi.start).mockImplementation(async body => {
      const queued = task({ trigger: "once", run_at: body.run_at! });
      vi.mocked(tasksApi.get).mockResolvedValue(taskView({ latest: [queued] })); return queued;
    });
    fireEvent.click(screen.getByRole("button", { name: "创建重建计划" })); await tick();
    expect(tasksApi.start).toHaveBeenCalledWith({ kind: "html_rebuild", run_at: "2026-10-02T04:30:00.000Z" });
    expect(screen.getByText(/重启后会继续调度/)).toBeTruthy();
  });

  it("拒绝DST歧义与超一年计划，不向服务器发送错误时刻", async () => {
    clock(); mount("America/New_York"); await tick();
    fireEvent.click(screen.getByLabelText("一次性计划"));
    const input = screen.getByLabelText("重建执行时间（America/New_York）");
    fireEvent.change(input, { target: { value: "2026-11-01T01:30" } });
    expect(screen.getByRole("alert").textContent).toContain("重复/不存在");
    expect(screen.getByRole("button", { name: "创建重建计划" }).hasAttribute("disabled")).toBe(true);
    fireEvent.change(input, { target: { value: "2028-10-01T12:00" } });
    expect(screen.getByRole("button", { name: "创建重建计划" }).hasAttribute("disabled")).toBe(true);
    expect(tasksApi.start).not.toHaveBeenCalled();
  });

  it("从latest恢复未来HTML计划，不受当前历史类型影响，取消后可以重设", async () => {
    const queued = task({ trigger: "once" });
    vi.mocked(tasksApi.get).mockResolvedValue(taskView({ latest: [queued], runs: { items: [task({ id: "publisher", kind: "publish_due", status: "completed", can_cancel: false })], next_cursor: null } }));
    vi.mocked(tasksApi.cancel).mockImplementation(async () => {
      const cancelled = task({ status: "cancelled", trigger: "once", can_cancel: false });
      vi.mocked(tasksApi.get).mockResolvedValue(taskView({ latest: [cancelled] })); return cancelled;
    });
    mount(); await screen.findByRole("button", { name: "等待执行" });
    expect(tasksApi.start).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "取消重建计划" }));
    await waitFor(() => expect(tasksApi.cancel).toHaveBeenCalledWith(queued.id));
    await screen.findByRole("button", { name: "开始重建" });
    expect(screen.getByRole("button", { name: "开始重建" }).hasAttribute("disabled")).toBe(false);
  });

  it("中断任务显式重试生成新编号并保留原记录", async () => {
    const interrupted = task({ status: "interrupted", can_retry: true, can_cancel: false });
    const retried = task({ id: "new-task", trigger: "retry", retry_of: interrupted.id });
    vi.mocked(tasksApi.get).mockResolvedValue(taskView({ latest: [interrupted], runs: { items: [interrupted], next_cursor: null } }));
    vi.mocked(tasksApi.retry).mockImplementation(async () => {
      vi.mocked(tasksApi.get).mockResolvedValue(taskView({ latest: [retried], runs: { items: [retried, interrupted], next_cursor: null } })); return retried;
    });
    mount(); fireEvent.click(await screen.findByRole("button", { name: "重新执行" }));
    await waitFor(() => expect(tasksApi.retry).toHaveBeenCalledWith(interrupted.id));
    expect(tasksApi.start).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("tab", { name: "任务记录" }));
    fireEvent.click(await screen.findByRole("button", { name: "查看任务 new-task" }));
    await screen.findByText(interrupted.id);
    expect(screen.getByText("原任务编号")).toBeTruthy();
  });

  it("部分失败展示已完成计数、类型与UUID，详情不会泄露原始内部错误", async () => {
    const failed = task({ status: "failed", can_cancel: false, can_retry: true, report: {
      html: report({ rebuilt: counts(4), failure: { kind: "page", id: "failure-page", message: "postgres://private/internal" } }),
      retention: null, publication: null, error: "postgres://private/internal",
    } });
    vi.mocked(tasksApi.get).mockResolvedValue(taskView({ latest: [failed], runs: { items: [failed], next_cursor: null } }));
    mount(); await screen.findByText(/失败内容：独立页面，编号：failure-page/);
    expect(screen.queryByText(/postgres:\/\/private/)).toBeNull();
    fireEvent.click(screen.getByRole("tab", { name: "任务记录" }));
    fireEvent.click(screen.getByRole("button", { name: `查看任务 ${failed.id}` }));
    expect(await screen.findByText("失败说明")).toBeTruthy();
    expect(screen.queryByText(/postgres:\/\/private/)).toBeNull();
  });

  it("保存周期计划携带版本及小时换算，服务器成功后清除未保存状态", async () => {
    mount(); fireEvent.click(await screen.findByRole("tab", { name: "清理计划" }));
    vi.mocked(tasksApi.saveRetentionSchedule).mockImplementation(async body => {
      const saved = schedule({ ...body, kind: "retention", version: 1 });
      vi.mocked(tasksApi.get).mockResolvedValue(taskView({ schedules: [saved] })); return saved;
    });
    fireEvent.click(screen.getByRole("checkbox", { name: "启用周期清理" }));
    fireEvent.change(screen.getByLabelText("清理间隔（小时）"), { target: { value: "48" } });
    fireEvent.blur(screen.getByLabelText("清理间隔（小时）"));
    await waitFor(() => expect(beforeUnloadBlocked()).toBe(true));
    fireEvent.click(screen.getByRole("button", { name: "保存清理计划" }));
    await waitFor(() => expect(tasksApi.saveRetentionSchedule).toHaveBeenCalledWith({ enabled: true, interval_seconds: 172_800, next_run_at: null, version: 0 }));
    await waitFor(() => expect(beforeUnloadBlocked()).toBe(false));
  });

  it("计划版本冲突保留输入，刷新不覆盖dirty配置，重新加载后提交新版本", async () => {
    vi.mocked(tasksApi.saveRetentionSchedule).mockRejectedValueOnce(new ApiError(409, "计划版本冲突", "version_conflict"));
    mount(); fireEvent.click(await screen.findByRole("tab", { name: "清理计划" }));
    const hours = screen.getByLabelText("清理间隔（小时）");
    fireEvent.change(hours, { target: { value: "48" } }); fireEvent.blur(hours);
    fireEvent.click(screen.getByRole("button", { name: "保存清理计划" }));
    await screen.findByText("计划版本冲突");
    vi.mocked(tasksApi.get).mockResolvedValue(taskView({ schedules: [schedule({ interval_seconds: 36_000, version: 3 })] }));
    fireEvent.click(screen.getByRole("button", { name: "刷新任务状态" }));
    await waitFor(() => expect(tasksApi.get).toHaveBeenCalledTimes(2));
    expect((hours as HTMLInputElement).value).toBe("48");
    fireEvent.click(screen.getByRole("button", { name: "重新加载计划并放弃修改" }));
    await waitFor(() => expect((hours as HTMLInputElement).value).toBe("10"));
    vi.mocked(tasksApi.saveRetentionSchedule).mockResolvedValue(schedule({ interval_seconds: 43_200, version: 4 }));
    fireEvent.change(hours, { target: { value: "12" } }); fireEvent.blur(hours);
    fireEvent.click(screen.getByRole("button", { name: "保存清理计划" }));
    await waitFor(() => expect(tasksApi.saveRetentionSchedule).toHaveBeenLastCalledWith(expect.objectContaining({ interval_seconds: 43_200, version: 3 })));
  });

  it("周期首次时间按站点时区提交UTC，单独修改间隔保留服务器秒数", async () => {
    clock();
    const first = schedule({ enabled: true, next_run_at: "2026-10-02T04:30:45Z", version: 2 });
    vi.mocked(tasksApi.get).mockResolvedValue(taskView({ schedules: [first] }));
    vi.mocked(tasksApi.saveRetentionSchedule).mockImplementation(async body => {
      const saved = schedule({ ...body, version: body.version + 1 });
      vi.mocked(tasksApi.get).mockResolvedValue(taskView({ schedules: [saved] })); return saved;
    });
    mount("Asia/Shanghai"); await tick(); fireEvent.click(screen.getByRole("tab", { name: "清理计划" }));
    const time = screen.getByLabelText("首次清理时间（Asia/Shanghai）");
    expect((time as HTMLInputElement).value).toBe("2026-10-02T12:30");
    const hours = screen.getByLabelText("清理间隔（小时）");
    fireEvent.change(hours, { target: { value: "36" } }); fireEvent.blur(hours);
    fireEvent.click(screen.getByRole("button", { name: "保存清理计划" })); await tick();
    expect(tasksApi.saveRetentionSchedule).toHaveBeenLastCalledWith({ enabled: true, interval_seconds: 129_600, next_run_at: "2026-10-02T04:30:45Z", version: 2 });
    fireEvent.change(time, { target: { value: "2026-10-03T12:30" } });
    fireEvent.click(screen.getByRole("button", { name: "保存清理计划" })); await tick();
    expect(tasksApi.saveRetentionSchedule).toHaveBeenLastCalledWith({ enabled: true, interval_seconds: 129_600, next_run_at: "2026-10-03T04:30:00.000Z", version: 3 });
  });

  it("清理环境不可用不能执行或开启，但可以停止已有计划", async () => {
    vi.mocked(tasksApi.get).mockResolvedValue(taskView({ retention_available: false, schedules: [schedule({ enabled: true, version: 6 })] }));
    vi.mocked(tasksApi.saveRetentionSchedule).mockResolvedValue(schedule({ enabled: false, version: 7 }));
    mount(); fireEvent.click(await screen.findByRole("tab", { name: "清理计划" }));
    expect(screen.getByText("当前未配置可用的清理执行环境")).toBeTruthy();
    expect(screen.getByRole("button", { name: "立即清理" }).hasAttribute("disabled")).toBe(true);
    fireEvent.click(screen.getByRole("checkbox", { name: "启用周期清理" }));
    fireEvent.click(screen.getByRole("button", { name: "保存清理计划" }));
    await waitFor(() => expect(tasksApi.saveRetentionSchedule).toHaveBeenCalledWith(expect.objectContaining({ enabled: false, version: 6 })));
    expect(tasksApi.start).not.toHaveBeenCalled();
  });

  it("历史按kind和cursor翻页，切换类型回首页，计划输入不因取数消失", async () => {
    vi.mocked(tasksApi.get).mockImplementation(async filter => taskView({ runs: {
      items: [task({ id: filter?.cursor ? "second-page" : "first-page" })], next_cursor: filter?.cursor ? null : "opaque/+cursor=",
    } }));
    mount(); fireEvent.click(await screen.findByRole("tab", { name: "清理计划" }));
    const hours = screen.getByLabelText("清理间隔（小时）");
    fireEvent.change(hours, { target: { value: "48" } }); fireEvent.blur(hours);
    fireEvent.click(screen.getByRole("tab", { name: "任务记录" }));
    fireEvent.click(screen.getByRole("button", { name: "下一页" }));
    await screen.findByRole("button", { name: "查看任务 second-page" });
    expect(tasksApi.get).toHaveBeenLastCalledWith({ kind: undefined, cursor: "opaque/+cursor=", limit: 20 }, expect.any(AbortSignal));
    fireEvent.mouseDown(screen.getByRole("combobox", { name: "任务类型筛选" }));
    fireEvent.click(await screen.findByTitle("保留期清理"));
    await waitFor(() => expect(tasksApi.get).toHaveBeenLastCalledWith({ kind: "retention", cursor: undefined, limit: 20 }, expect.any(AbortSignal)));
    fireEvent.click(screen.getByRole("tab", { name: "清理计划" }));
    expect((screen.getByLabelText("清理间隔（小时）") as HTMLInputElement).value).toBe("48");
    expect(beforeUnloadBlocked()).toBe(true);
  });

  it("运行时2秒更新、完成后恢复10秒更新，GET失败停止直到手动刷新", async () => {
    clock();
    const running = task({ status: "running", can_cancel: false, report: { html: report(), retention: null, publication: null, error: null } });
    vi.mocked(tasksApi.get).mockResolvedValueOnce(taskView({ latest: [running], pending_html: null }))
      .mockRejectedValueOnce(new TypeError("Failed to fetch"))
      .mockResolvedValue(taskView({ latest: [task({ status: "completed", can_cancel: false, report: { html: report({ has_more: false }), retention: null, publication: null, error: null } })] }));
    mount(); await tick();
    expect(screen.queryByRole("button", { name: "取消重建计划" })).toBeNull();
    await tick(2_000); expect(screen.getByText(/自动更新已暂停/)).toBeTruthy();
    await tick(30_000); expect(tasksApi.get).toHaveBeenCalledTimes(2);
    fireEvent.click(screen.getByRole("button", { name: "刷新任务状态" })); await tick();
    expect(tasksApi.get).toHaveBeenCalledTimes(3);
    await tick(2_000); expect(tasksApi.get).toHaveBeenCalledTimes(3);
    await tick(8_000); expect(tasksApi.get).toHaveBeenCalledTimes(4);
  });

  it("无settings.manage不查询也不提供任务操作", async () => {
    auth.permissions = ["post.read"]; mount();
    await screen.findByText("当前账号没有管理任务的权限。");
    expect(tasksApi.get).not.toHaveBeenCalled(); expect(tasksApi.start).not.toHaveBeenCalled();
  });

  it.each([counts(), null])("没有或未知待重建数量时不允许创建任务 %#", async pending => {
    vi.mocked(tasksApi.get).mockResolvedValue(taskView({ pending_html: pending }));
    mount(); await screen.findByRole("button", { name: "开始重建" });
    expect(screen.getByRole("button", { name: "开始重建" }).hasAttribute("disabled")).toBe(true);
    expect(tasksApi.start).not.toHaveBeenCalled();
  });

  it("重建完成失效评论HTML缓存，同一终态重复GET不会反复失效", async () => {
    const queued = task(); const completed = task({ status: "completed", can_cancel: false,
      report: { html: report({ rebuilt: counts(0, 0, 2), has_more: false }), retention: null, publication: null, error: null } });
    vi.mocked(tasksApi.get).mockResolvedValueOnce(taskView({ latest: [queued] })).mockResolvedValue(taskView({ latest: [completed] }));
    let client!: QueryClient;
    mount("UTC", current => { client = current; }); await screen.findByRole("button", { name: "等待执行" });
    client.setQueryData(queryKeys.comments(1, "all"), { content_html: "旧HTML" });
    fireEvent.click(screen.getByRole("button", { name: "刷新任务状态" }));
    await screen.findByText("内容重建完成。");
    expect(client.getQueryState(queryKeys.comments(1, "all"))?.isInvalidated).toBe(true);
    client.setQueryData(queryKeys.comments(1, "all"), { content_html: "已更新HTML" });
    fireEvent.click(screen.getByRole("button", { name: "刷新任务状态" }));
    await waitFor(() => expect(tasksApi.get).toHaveBeenCalledTimes(3));
    expect(client.getQueryState(queryKeys.comments(1, "all"))?.isInvalidated).toBe(false);
  });

  it("记录GET返回403时隐藏旧详情并提供权限反馈", async () => {
    const old = task({ status: "completed", can_cancel: false });
    vi.mocked(tasksApi.get).mockResolvedValueOnce(taskView({ runs: { items: [old], next_cursor: null } }))
      .mockRejectedValue(new ApiError(403, "需要站点设置权限", "forbidden"));
    mount(); fireEvent.click(await screen.findByRole("tab", { name: "任务记录" }));
    fireEvent.click(screen.getByRole("button", { name: `查看任务 ${old.id}` }));
    expect(within(await screen.findByRole("dialog")).getByText("任务详情")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "刷新任务状态" }));
    await screen.findByText("没有权限：需要站点设置权限");
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(screen.queryByRole("button", { name: `查看任务 ${old.id}` })).toBeNull();
    vi.mocked(tasksApi.get).mockResolvedValue(taskView({ runs: { items: [old], next_cursor: null } }));
    fireEvent.click(screen.getByRole("button", { name: "刷新任务状态" }));
    await screen.findByRole("button", { name: `查看任务 ${old.id}` });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("恢复隔离禁用所有写入，仍可查看已有记录", async () => {
    vi.mocked(tasksApi.get).mockResolvedValue(taskView({ available: false, latest: [task()] }));
    mount(); await screen.findByRole("button", { name: "等待执行" });
    expect(screen.getByRole("button", { name: "取消重建计划" }).hasAttribute("disabled")).toBe(true);
    expect(screen.getByRole("button", { name: "等待执行" }).hasAttribute("disabled")).toBe(true);
    expect(tasksApi.cancel).not.toHaveBeenCalled();
  });

  it("已发送任务在离开后仍提交，返回读取服务端状态而不重复POST", async () => {
    let finish!: (run: TaskRun) => void;
    vi.mocked(tasksApi.start).mockReturnValue(new Promise(resolve => { finish = resolve; }));
    const mounted = mount(); fireEvent.click(await screen.findByRole("button", { name: "开始重建" }));
    await waitFor(() => expect(tasksApi.start).toHaveBeenCalledTimes(1));
    mounted.rerender(<AdminProviders><div>其它页面</div></AdminProviders>);
    vi.mocked(tasksApi.get).mockResolvedValue(taskView({ latest: [task()] }));
    await act(async () => finish(task()));
    expect(screen.getByText("其它页面")).toBeTruthy();
    expect(screen.queryByText(/任务已提交/)).toBeNull();
    mounted.rerender(<AdminProviders><TaskManagementScreen /></AdminProviders>);
    await screen.findByRole("button", { name: "等待执行" });
    expect(tasksApi.start).toHaveBeenCalledTimes(1);
  });
});
