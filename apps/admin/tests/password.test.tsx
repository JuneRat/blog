// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * 自助改密弹窗回归（`POST /api/admin/v1/me/password`）：
 * - 成功：载荷正确、轮换后的新 csrf_token 写回内存、提示并关闭；
 * - 两次新密码不一致只在本地拦下（不发请求，也不复制服务端口令策略）；
 * - 403 invalid_credentials 是「当前密码错」而不是「掉线」，不清登录态；
 * - 409 version_conflict 明确说明本次作废、当前口令已失效；
 * - 当前密码留空时请求体里不带该字段（OAuth 用户设置初始密码的路径）。
 *
 * 失败用例只断言「没有登出行为」：改密失败时用户仍然是登录的。
 */
const auth = vi.hoisted(() => ({ logout: vi.fn(), refresh: vi.fn() }));

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    status: "authenticated",
    me: null,
    logout: auth.logout,
    refresh: auth.refresh,
    logoutError: null,
  }),
}));

vi.mock("../src/api/identity", async (load) => {
  const original = await load<typeof import("../src/api/identity")>();
  return { ...original, identityApi: { ...original.identityApi, changeOwnPassword: vi.fn() } };
});
vi.mock("../src/api/client", async (load) => {
  const original = await load<typeof import("../src/api/client")>();
  return { ...original, setCsrfToken: vi.fn() };
});

import { ApiError, setCsrfToken } from "../src/api/client";
import { identityApi } from "../src/api/identity";
import { PasswordChangeModal } from "../src/components/PasswordChangeModal";
import { AdminProviders } from "../src/providers";

const changeOwnPassword = vi.mocked(identityApi.changeOwnPassword);
const setToken = vi.mocked(setCsrfToken);

/** antd 的 Form.Item 用 label 关联输入框；两字中文按钮的空格已在 AdminProviders 关闭。 */
function field(label: string): HTMLInputElement {
  return screen.getByLabelText(label) as HTMLInputElement;
}

function renderModal() {
  const onClose = vi.fn();
  render(
    <AdminProviders>
      <PasswordChangeModal open onClose={onClose} />
    </AdminProviders>,
  );
  return { onClose };
}

/** 填表并提交；`onFinish` 在异步校验之后才触发，调用方用 waitFor/findBy 等结果。 */
function fill(current: string, next: string, confirm: string = next): void {
  if (current !== "") {
    fireEvent.change(field("当前密码"), { target: { value: current } });
  }
  fireEvent.change(field("新密码"), { target: { value: next } });
  fireEvent.change(field("确认新密码"), { target: { value: confirm } });
  fireEvent.click(screen.getByRole("button", { name: "更新密码" }));
}

beforeEach(() => {
  vi.resetAllMocks();
});

afterEach(cleanup);

describe("自助改密弹窗", () => {
  it("成功后写回新 csrf_token、给出成功提示并关闭", async () => {
    changeOwnPassword.mockResolvedValue({ user_id: "u1", csrf_token: "csrf-new" });
    const { onClose } = renderModal();

    fill("old-pass", "new-pass-1");

    await waitFor(() =>
      expect(changeOwnPassword).toHaveBeenCalledWith({
        current_password: "old-pass",
        new_password: "new-pass-1",
      }),
    );
    // 会话已轮换：旧 token 立即失效，不写回新值会让后续写请求全部 403。
    expect(setToken).toHaveBeenCalledWith("csrf-new");
    expect(onClose).toHaveBeenCalledTimes(1);
    await screen.findByText(/密码已更新/);
    // 表单已重置，再次打开不会带上一次的密码草稿。
    expect(field("当前密码").value).toBe("");
    expect(field("新密码").value).toBe("");
    expect(field("确认新密码").value).toBe("");
  });

  it("两次新密码不一致：本地拦下，不发请求", async () => {
    renderModal();

    fill("old-pass", "new-pass-1", "new-pass-2");

    await screen.findByText("两次输入的新密码不一致");
    expect(changeOwnPassword).not.toHaveBeenCalled();
  });

  it("403 invalid_credentials：展示服务端文案与请求编号，弹窗保持打开且不登出", async () => {
    changeOwnPassword.mockRejectedValue(
      new ApiError(403, "凭据无效", "invalid_credentials", "req-403"),
    );
    const { onClose } = renderModal();

    fill("wrong-pass", "new-pass-1");

    await screen.findByText(/当前密码不正确/);
    expect(screen.getByText(/凭据无效/)).toBeTruthy();
    expect(screen.getByText(/req-403/)).toBeTruthy();
    // 用户没有掉线：不清 token、不刷新会话、不登出，弹窗不关，允许改正重试。
    expect(setToken).not.toHaveBeenCalled();
    expect(auth.logout).not.toHaveBeenCalled();
    expect(auth.refresh).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "更新密码" })).toBeTruthy();
  });

  it("409 version_conflict：提示本次作废、当前口令已失效", async () => {
    changeOwnPassword.mockRejectedValue(
      new ApiError(409, "版本冲突：口令已被并发修改", "version_conflict", "req-409"),
    );
    const { onClose } = renderModal();

    fill("old-pass", "new-pass-1");

    await screen.findByText(/版本冲突：口令已被并发修改/);
    expect(screen.getByText(/当前口令已失效/)).toBeTruthy();
    expect(setToken).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
  });

  it("当前密码留空：请求体不含 current_password（OAuth 用户设置初始密码）", async () => {
    changeOwnPassword.mockResolvedValue({ user_id: "u1", csrf_token: "csrf-new" });
    renderModal();

    fill("", "new-pass-1");

    await waitFor(() => expect(changeOwnPassword).toHaveBeenCalledTimes(1));
    const payload = changeOwnPassword.mock.calls[0]?.[0];
    expect(payload?.new_password).toBe("new-pass-1");
    expect(payload?.current_password).toBeUndefined();
    // 直接锁线上报文：JSON.stringify 会丢掉 undefined，请求体里不能出现该键。
    expect(JSON.stringify(payload)).not.toContain("current_password");
  });
});
