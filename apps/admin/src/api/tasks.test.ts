import { afterEach, describe, expect, it, vi } from "vitest";
import { tasksApi } from "./tasks";
import { ApiProtocolError, setCsrfToken } from "./client";
import { jsonResponse } from "../../tests/httpFixtures";
import { schedule, task, taskView } from "../../tests/taskFixtures";

afterEach(() => { vi.unstubAllGlobals(); setCsrfToken(null); });
function respond(value: unknown, status = 200) {
  const fetcher = vi.fn().mockResolvedValue(jsonResponse(value, status));
  vi.stubGlobal("fetch", fetcher); return fetcher;
}
describe("持久化任务API", () => {
  it("GET精确编码kind与不透明cursor，默认20条并保留latest", async () => {
    const fetcher = respond(taskView({ latest: [task()] }));
    await expect(tasksApi.get({ kind: "html_rebuild", cursor: "/+?cursor=" })).resolves.toMatchObject({ latest: [task()] });
    const url = new URL(fetcher.mock.calls[0][0], "http://localhost");
    expect(url.searchParams.get("cursor")).toBe("/+?cursor=");
    expect(url.searchParams.get("kind")).toBe("html_rebuild");
    expect(url.searchParams.get("limit")).toBe("20");
  });
  it("202启动提交kind/run_at并使用标准CSRF", async () => {
    setCsrfToken("task-csrf"); const fetcher = respond(task(), 202);
    await tasksApi.start({ kind: "html_rebuild", run_at: "2026-10-02T00:00:00Z" });
    expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({ kind: "html_rebuild", run_at: "2026-10-02T00:00:00Z" });
    expect(fetcher.mock.calls[0][1].headers.get("X-CSRF-Token")).toBe("task-csrf");
    expect(fetcher.mock.calls[0][1].credentials).toBe("same-origin");
  });
  it.each(["retry", "cancel"] as const)("%s无需body并编码任务编号", async operation => {
    const fetcher = respond(task(), operation === "retry" ? 202 : 200);
    await tasksApi[operation]("id/with space");
    expect(fetcher.mock.calls[0][0]).toBe(`/api/admin/v1/tasks/id%2Fwith%20space/${operation}`);
    expect(fetcher.mock.calls[0][1].method).toBe("POST");
    expect(fetcher.mock.calls[0][1].body).toBeUndefined();
  });
  it("保存计划明确携带版本、固定秒数间隔与nullable首次时间", async () => {
    const fetcher = respond(schedule({ enabled: true, version: 4 }));
    await tasksApi.saveRetentionSchedule({ enabled: true, interval_seconds: 86_400, next_run_at: null, version: 3 });
    expect(fetcher.mock.calls[0][1].method).toBe("PUT");
    expect(JSON.parse(fetcher.mock.calls[0][1].body)).toEqual({ enabled: true, interval_seconds: 86_400, next_run_at: null, version: 3 });
  });
  it.each([
    { ...taskView(), latest: [{ ...task(), status: "unknown" }] },
    { ...taskView(), latest: [{ ...task(), kind: "media" }] },
    { ...taskView(), latest: [{ ...task(), started_at: undefined }] },
    { ...taskView(), schedules: [{ ...schedule(), interval_seconds: -1 }] },
  ])("拒绝畸形任务契约 %#", async payload => {
    respond(payload); await expect(tasksApi.get()).rejects.toBeInstanceOf(ApiProtocolError);
  });
  it("403保留权限错误和请求编号", async () => {
    respond({ error: "需要站点设置权限", code: "forbidden" }, 403);
    await expect(tasksApi.start({ kind: "retention", run_at: null })).rejects.toMatchObject({ status: 403, code: "forbidden", requestId: "request-1" });
  });
});
