// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { TimeZoneContext } from "../src/timeZoneContext";
import { App } from "../src/App";
import { ApiError } from "../src/api/client";
import { auditApi } from "../src/api/audit";
import type { AuditRecord } from "../src/types";

const auth = vi.hoisted(() => ({ permissions: ["audit.read"] as string[] }));
vi.mock("../src/auth", () => ({ useAuth: () => ({ status: "authenticated", me: { user_id: "owner", permissions: auth.permissions } }) }));
vi.mock("../src/api/audit", async (load) => {
  const original = await load<typeof import("../src/api/audit")>();
  return { ...original, auditApi: { list: vi.fn() } };
});
const item: AuditRecord = {
  id: "log-1", created_at: "2026-09-27T10:00:00Z", actor_id: "former-user", actor_display: null,
  ip_address: "2001:db8::42", action: "post.update", target_type: "post", target_id: "post-1",
  summary: [{ key: "changed", value: "<img src=x onerror=alert(1)>" }],
};
beforeEach(() => {
  auth.permissions = ["audit.read"];
  window.history.replaceState(null, "", "/admin/audit-logs");
  vi.mocked(auditApi.list).mockResolvedValue({ items: [item], next_cursor: "cursor-1" });
});
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("keeps settings administrators out of the audit query", async () => {
  auth.permissions = ["settings.manage"];
  render(<App />);
  await screen.findByText("当前账号没有查看审计日志的权限。");
  expect(auditApi.list).not.toHaveBeenCalled();
});

it("renders historical identity and metadata as text and distinguishes anonymous records", async () => {
  vi.mocked(auditApi.list).mockResolvedValue({ items: [item, { ...item, id: "log-2", actor_id: null, ip_address: null }], next_cursor: null });
  render(<App />);
  await screen.findByText("账号已不存在");
  expect(screen.getByText("无关联账号")).toBeTruthy();
  fireEvent.click(within(screen.getByRole("row", { name: /账号已不存在/ })).getByRole("button", { name: "查看 post.update 详情" }));
  const modal = await screen.findByRole("dialog");
  expect(within(modal).getByText(/former-user/)).toBeTruthy();
  expect(within(modal).getByText("<img src=x onerror=alert(1)>")).toBeTruthy();
  expect(modal.querySelector("img,script")).toBeNull();
});

it("uses the returned boundary and resets pagination when applying filters or refreshing", async () => {
  vi.mocked(auditApi.list).mockImplementation(async (_, cursor) => ({ items: [{ ...item, id: cursor ?? "first" }], next_cursor: cursor ? null : "cursor-1" }));
  render(<App />);
  await screen.findByText("post.update");
  fireEvent.click(screen.getByRole("button", { name: "下一页" }));
  await waitFor(() => expect(auditApi.list).toHaveBeenLastCalledWith({}, "cursor-1"));
  await screen.findByText("第 2 页");
  fireEvent.change(screen.getByLabelText("动作"), { target: { value: " settings.site " } });
  fireEvent.click(screen.getByRole("button", { name: /筛\s*选/ }));
  await waitFor(() => expect(auditApi.list).toHaveBeenLastCalledWith({ action: "settings.site" }, undefined));
  await screen.findByText("第 1 页");
  fireEvent.click(screen.getByRole("button", { name: "下一页" }));
  await waitFor(() => expect(auditApi.list).toHaveBeenLastCalledWith({ action: "settings.site" }, "cursor-1"));
  await waitFor(() => expect(screen.getByRole("button", { name: /刷\s*新/ }).hasAttribute("disabled")).toBe(false));
  fireEvent.click(screen.getByRole("button", { name: /刷\s*新/ }));
  await waitFor(() => expect(auditApi.list).toHaveBeenLastCalledWith({ action: "settings.site" }, undefined));
});

it("rejects inverted dates, replaces actor ID with the unassociated filter, and shows query failures", async () => {
  render(<App />);
  await screen.findByText("post.update");
  fireEvent.change(screen.getByLabelText("开始时间（含）"), { target: { value: "2026-09-28T00:00" } });
  fireEvent.change(screen.getByLabelText("结束时间（不含）"), { target: { value: "2026-09-27T00:00" } });
  fireEvent.click(screen.getByRole("button", { name: /筛\s*选/ }));
  await screen.findByText("结束时间必须晚于开始时间。");
  expect(auditApi.list).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByRole("button", { name: /重\s*置/ }));
  await waitFor(() => expect(screen.getByRole("button", { name: /刷\s*新/ }).hasAttribute("disabled")).toBe(false));
  fireEvent.change(screen.getByLabelText("操作者 ID"), { target: { value: "someone" } });
  fireEvent.click(screen.getByRole("checkbox"));
  vi.mocked(auditApi.list).mockRejectedValue(new ApiError(403, "权限已撤销", "forbidden"));
  fireEvent.click(screen.getByRole("button", { name: /筛\s*选/ }));
  await waitFor(() => expect(auditApi.list).toHaveBeenLastCalledWith({ without_actor: true }, undefined));
  await screen.findByText(/权限已撤销/);
  expect(screen.queryByText("账号已不存在")).toBeNull();
  expect(screen.getByRole("button", { name: "下一页" }).hasAttribute("disabled")).toBe(true);
});

it("displays audit timestamps and sends filter bounds in the configured site zone", async () => {
  render(<TimeZoneContext.Provider value="Asia/Shanghai"><App /></TimeZoneContext.Provider>);
  await screen.findByText("2026-09-27 18:00:00 +08:00 (Asia/Shanghai)");
  fireEvent.change(screen.getByLabelText("开始时间（含）"), { target: { value: "2026-09-28T00:00" } });
  fireEvent.change(screen.getByLabelText("结束时间（不含）"), { target: { value: "2026-09-29T00:00" } });
  fireEvent.click(screen.getByRole("button", { name: /筛\s*选/ }));
  await waitFor(() => expect(auditApi.list).toHaveBeenLastCalledWith({
    from: "2026-09-27T16:00:00.000Z", until: "2026-09-28T16:00:00.000Z",
  }, undefined));
});
