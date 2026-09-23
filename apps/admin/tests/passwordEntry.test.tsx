// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { api } from "../src/api";
import { paths } from "../src/router";

/**
 * 自助改密的**入口接线**：AdminLayout 右上角「修改密码」打开 PasswordChangeModal。
 *
 * 弹窗自身的行为在 tests/password.test.tsx 里覆盖；这里只验证外壳里的入口
 * 真的挂上了（并因此处在 AdminProviders 的 antd App 上下文内——
 * 少了那层，`App.useApp()` 拿不到 modal/message，弹窗点了没反应）。
 */

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    status: "authenticated",
    me: { user_id: "me", permissions: ["tag.manage"] },
  }),
}));

vi.mock("../src/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("../src/api")>();
  return {
    ...original,
    api: {
      listTags: vi.fn(),
      changeOwnPassword: vi.fn(),
    },
  };
});

beforeEach(() => {
  vi.resetAllMocks();
  window.history.replaceState(null, "", paths.tags);
  vi.mocked(api.listTags).mockResolvedValue([]);
});
afterEach(cleanup);

describe("自助改密入口", () => {
  it("外壳右上角打开改密弹窗，提交后调用改密接口", async () => {
    vi.mocked(api.changeOwnPassword).mockResolvedValue({ user_id: "me", csrf_token: "csrf-new" });
    render(<App />);
    // 等懒加载的屏幕挂载，确认外壳与内容都在。
    await screen.findByText(/还没有标签/);

    fireEvent.click(screen.getByRole("button", { name: "修改密码" }));
    const dialog = await screen.findByRole("dialog", { name: /修改密码/ });
    expect(dialog).toBeTruthy();

    fireEvent.change(screen.getByLabelText("当前密码"), { target: { value: "old-secret" } });
    fireEvent.change(screen.getByLabelText("新密码"), { target: { value: "new-secret-value" } });
    fireEvent.change(screen.getByLabelText("确认新密码"), { target: { value: "new-secret-value" } });
    fireEvent.click(screen.getByRole("button", { name: "更新密码" }));

    await waitFor(() =>
      expect(api.changeOwnPassword).toHaveBeenCalledWith({
        current_password: "old-secret",
        new_password: "new-secret-value",
      }),
    );
  });
});
