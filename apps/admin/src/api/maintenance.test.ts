import { afterEach, describe, expect, it, vi } from "vitest";
import { maintenanceApi } from "./maintenance";
import { ApiProtocolError, setCsrfToken } from "./client";
import { jsonResponse } from "../../tests/httpFixtures";
import { counts, job, view } from "../../tests/maintenanceFixtures";

afterEach(() => {
  vi.unstubAllGlobals();
  setCsrfToken(null);
});

describe("内容维护 API", () => {
  it("只读GET解析待重建数量与已有任务，并传递查询取消信号", async () => {
    const payload = view({ job: job() });
    const fetcher = vi.fn().mockResolvedValue(jsonResponse(payload));
    vi.stubGlobal("fetch", fetcher);
    const controller = new AbortController();
    await expect(maintenanceApi.get(controller.signal)).resolves.toEqual(payload);
    expect(fetcher.mock.calls[0][0]).toBe("/api/admin/v1/maintenance/html-rebuild");
    expect(fetcher.mock.calls[0][1].method).toBeUndefined();
    controller.abort();
    expect(fetcher.mock.calls[0][1].signal.aborted).toBe(true);
  });

  it("POST接受202任务响应，不发送body且使用现有CSRF与同源凭据", async () => {
    setCsrfToken("maintenance-csrf");
    const fetcher = vi.fn().mockResolvedValue(jsonResponse(job(), 202));
    vi.stubGlobal("fetch", fetcher);
    await expect(maintenanceApi.start()).resolves.toEqual(job());
    const init = fetcher.mock.calls[0][1];
    expect(init.method).toBe("POST");
    expect(init.body).toBeUndefined();
    expect(init.headers.get("X-CSRF-Token")).toBe("maintenance-csrf");
    expect(init.headers.has("Content-Type")).toBe(false);
    expect(init.credentials).toBe("same-origin");
  });

  it.each([
    view({ pending: counts(-1) }),
    { ...view(), job: { ...job(), status: "queued" } },
    { available: true, pending: counts() },
    { ...view(), job: { ...job(), report: { ...job().report, failure: { kind: "media", id: null, message: "failed" } } } },
  ])("拒绝不兼容的维护响应 %#", async payload => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(jsonResponse(payload)));
    await expect(maintenanceApi.get()).rejects.toBeInstanceOf(ApiProtocolError);
  });

  it("403保留权限错误而不当作成功响应解析", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(jsonResponse({ error: "需要站点设置权限", code: "forbidden" }, 403)));
    await expect(maintenanceApi.start()).rejects.toMatchObject({ status: 403, code: "forbidden" });
  });
});
