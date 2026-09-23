import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const { refresh } = vi.hoisted(() => ({ refresh: vi.fn(async () => {}) }));

vi.mock("../auth", () => ({
  useAuth: () => ({
    status: "anonymous",
    me: null,
    providers: [{ id: "idp", name: "示例 IdP", kind: "oidc" }],
    providersLoaded: true,
    logoutError: null,
    refresh,
    logout: async () => {},
    goToLogin: async () => {},
  }),
}));

vi.mock("../api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api")>();
  return { ...actual, api: { loginWithPassword: vi.fn() } };
});

import { ApiError, api } from "../api";
import { AdminProviders } from "../providers";
import { LoginScreen } from "./LoginScreen";

const loginWithPassword = vi.mocked(api.loginWithPassword);

/**
 * 登录屏在真实应用里总处于 AdminProviders 内（主题、zh_CN locale、antd App 上下文）。
 * 测试直接渲染屏幕时补上同样的外壳，否则会拿到 antd 的英文默认 locale 与插空格行为。
 */
function renderLogin(): ReturnType<typeof render> {
  return render(
    <AdminProviders>
      <LoginScreen />
    </AdminProviders>,
  );
}

function submitButton(): HTMLButtonElement {
  return screen.getByRole("button", { name: "登录" }) as HTMLButtonElement;
}

function fillForm(username: string, password: string): void {
  fireEvent.change(screen.getByLabelText("用户名"), { target: { value: username } });
  fireEvent.change(screen.getByLabelText("密码"), { target: { value: password } });
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("LoginScreen", () => {
  it("submits trimmed credentials and refreshes the session on success", async () => {
    loginWithPassword.mockResolvedValueOnce({ user_id: "u1", next: "/admin/" });
    renderLogin();

    fillForm("  sun ", "harbor-lantern-2026");
    fireEvent.click(submitButton());

    await waitFor(() => expect(refresh).toHaveBeenCalledTimes(1));
    expect(loginWithPassword).toHaveBeenCalledWith({
      username: "sun",
      password: "harbor-lantern-2026",
      next: "/admin/",
    });
  });

  it("shows the server error with request id and clears the password", async () => {
    loginWithPassword.mockRejectedValueOnce(
      new ApiError(401, "用户名或密码不正确", "invalid_credentials", "req-1"),
    );
    renderLogin();

    fillForm("sun", "wrong-password-value");
    fireEvent.click(submitButton());

    expect(await screen.findByText(/用户名或密码不正确/)).toBeTruthy();
    expect(screen.getByText(/req-1/)).toBeTruthy();
    expect((screen.getByLabelText("密码") as HTMLInputElement).value).toBe("");
    expect(refresh).not.toHaveBeenCalled();
  });

  it("提交过程中重复提交只发一次请求", async () => {
    let resolveLogin: (value: { user_id: string; next: string }) => void = () => {};
    loginWithPassword.mockReturnValueOnce(
      new Promise((resolve) => {
        resolveLogin = resolve;
      }),
    );
    renderLogin();

    fillForm("sun", "harbor-lantern-2026");
    const button = submitButton();
    // 按钮 disabled 要等下一次渲染；同步闸门必须在同一 tick 内挡住重复提交。
    fireEvent.click(button);
    fireEvent.click(button);
    fireEvent.submit(button.closest("form") as HTMLFormElement);
    // antd Form 的 onFinish 在异步校验之后才跑，三个触发都排进微任务队列，
    // 同步闸门只能放过第一个。
    await waitFor(() => expect(loginWithPassword).toHaveBeenCalledTimes(1));
    await act(async () => {});
    expect(loginWithPassword).toHaveBeenCalledTimes(1);

    resolveLogin({ user_id: "u1", next: "/admin/" });
    await waitFor(() => expect(refresh).toHaveBeenCalledTimes(1));
  });

  it("keeps submit disabled until both fields are filled", () => {
    renderLogin();
    expect(submitButton().disabled).toBe(true);

    fillForm("sun", "harbor-lantern-2026");
    expect(submitButton().disabled).toBe(false);
  });

  it("renders provider buttons from the public provider list", () => {
    renderLogin();
    const link = screen.getByRole("link", { name: "使用 示例 IdP 登录" }) as HTMLAnchorElement;
    expect(link.getAttribute("href")).toBe("/auth/login?provider=idp&next=%2Fadmin%2F");
  });
});
