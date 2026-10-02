// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { identityApi } from "../src/api/identity";
const auth = vi.hoisted(() => ({ status: "unauthenticated", providers: [], providersLoaded: true }));
vi.mock("../src/auth", () => ({ useAuth: () => auth }));
vi.mock("../src/api/identity", async (load) => {
  const original = await load<typeof import("../src/api/identity")>();
  return { ...original, identityApi: { ...original.identityApi, registrationStatus: vi.fn(), requestPasswordRecovery: vi.fn(), resetPassword: vi.fn() } };
});
beforeEach(() => {
  vi.resetAllMocks();
  auth.status = "unauthenticated";
  window.history.replaceState(null, "", "/admin/");
  vi.mocked(identityApi.registrationStatus).mockResolvedValue({ enabled: false });
});
afterEach(cleanup);
it("finds recovery from login, sends only the email and displays the generic result", async () => {
  vi.mocked(identityApi.requestPasswordRecovery).mockResolvedValue({ message: "如果该邮箱关联可找回的账号，将收到邮件。" });
  render(<App />);
  fireEvent.click(screen.getByRole("button", { name: "忘记密码" }));
  fireEvent.change(screen.getByLabelText("账号邮箱"), { target: { value: "member@example.com" } });
  fireEvent.click(screen.getByRole("button", { name: "发送重置邮件" }));
  await screen.findByText("如果该邮箱关联可找回的账号，将收到邮件。");
  expect(identityApi.requestPasswordRecovery).toHaveBeenCalledWith({ email: "member@example.com" });
  expect(identityApi.resetPassword).not.toHaveBeenCalled();
});
it("opens an email fragment, immediately removes it from history and resets without logging in", async () => {
  const token = "a".repeat(64);
  window.history.replaceState(null, "", `/admin/#password-reset=${token}`);
  vi.mocked(identityApi.resetPassword).mockResolvedValue({ message: "密码已设置，请重新登录。" });
  render(<App />);
  expect(window.location.hash).toBe("");
  fireEvent.change(screen.getByLabelText("新密码"), { target: { value: "harbor-lantern-2026" } });
  fireEvent.change(screen.getByLabelText("再次输入新密码"), { target: { value: "harbor-lantern-2026" } });
  fireEvent.click(screen.getByRole("button", { name: "设置密码" }));
  await screen.findByText("密码已设置，请重新登录。");
  expect(identityApi.resetPassword).toHaveBeenCalledWith({ token, password: "harbor-lantern-2026" });
  expect(screen.queryByLabelText("新密码")).toBeNull();
});
it("keeps an expired-link error visible and clears password fields on failure", async () => {
  window.history.replaceState(null, "", `/admin/#password-reset=${"b".repeat(64)}`);
  vi.mocked(identityApi.resetPassword).mockRejectedValue(new Error("链接无效或已过期，请重新申请邮件。"));
  render(<App />);
  fireEvent.change(screen.getByLabelText("新密码"), { target: { value: "harbor-lantern-2026" } });
  fireEvent.change(screen.getByLabelText("再次输入新密码"), { target: { value: "harbor-lantern-2026" } });
  fireEvent.click(screen.getByRole("button", { name: "设置密码" }));
  await screen.findByText("链接无效或已过期，请重新申请邮件。");
  await waitFor(() => expect((screen.getByLabelText("新密码") as HTMLInputElement).value).toBe(""));
});

it("keeps the email token when the initial session check remounts providers", async () => {
  const token = "c".repeat(64);
  window.history.replaceState(null, "", `/admin/#password-reset=${token}`);
  auth.status = "loading";
  const view = render(<App />);
  expect(window.location.hash).toBe("");
  auth.status = "unauthenticated";
  view.rerender(<App />);
  expect(screen.getByRole("heading", { name: "设置登录密码" })).toBeTruthy();
  vi.mocked(identityApi.resetPassword).mockResolvedValue({ message: "密码已设置，请重新登录。" });
  fireEvent.change(screen.getByLabelText("新密码"), { target: { value: "harbor-lantern-2026" } });
  fireEvent.change(screen.getByLabelText("再次输入新密码"), { target: { value: "harbor-lantern-2026" } });
  fireEvent.click(screen.getByRole("button", { name: "设置密码" }));
  await waitFor(() => expect(identityApi.resetPassword).toHaveBeenCalledWith({ token, password: "harbor-lantern-2026" }));
});
