// @vitest-environment jsdom
import { meResponse } from "./httpFixtures";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError } from "../src/api/client";
import { identityApi } from "../src/api/identity";
import { paths } from "../src/router";
import type { AdminUser, Me, RoleSummary, UserPage } from "../src/types";

/**
 * 用户与角色管理界面回归：
 * - 权限边界：无 user.manage/role.manage 时不发起任何账号请求；
 * - 冲突码消费者：username_taken / email_taken 定位到具体文案；
 * - 最后可登录 Admin：列表标记 + 禁用移除，后端 last_admin 也有专属文案；
 * - 改自己的角色成功后主动刷新 `/me` 的权限与资料版本。
 *
 * `auth` 用可变 hoisted 对象，便于每个用例切换权限与当前用户。
 */
const auth = vi.hoisted(() => ({
  me: null as Me | null,
  refresh: vi.fn(async () => {}),
}));

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    status: "authenticated",
    me: auth.me,
    refresh: auth.refresh,
    logout: vi.fn(),
    logoutError: null,
  }),
}));

vi.mock("../src/api/identity", async (load) => {
  const original = await load<typeof import("../src/api/identity")>();
  return { ...original, identityApi: { ...original.identityApi, listUsers: vi.fn(), createUser: vi.fn(), listRoles: vi.fn(), assignRole: vi.fn(), removeRole: vi.fn(), changeUserStatus: vi.fn() } };
});

function me(permissions: string[], userId = "u-me"): Me {
  return meResponse({ user_id: userId, username: "me", permissions });
}

function user(overrides: Partial<AdminUser> = {}): AdminUser {
  return {
    id: "u-author",
    username: "author",
    email: null,
    display_name: "作者",
    status: "active",
    version: 7,
    deleted: false,
    can_login: true,
    is_last_loginable_admin: false,
    password_enabled: true,
    external_identities: 0,
    roles: ["author"],
    ...overrides,
  };
}

function userPage(items: AdminUser[], total = items.length, page = 1, per_page = 50): UserPage {
  return { items, total, page, per_page };
}

const roles: RoleSummary[] = [
  { slug: "admin", name: "Administrator", description: null, builtin: true, permission_count: 21 },
  { slug: "reader", name: "Reader", description: null, builtin: true, permission_count: 0 },
  { slug: "editor", name: "Editor", description: null, builtin: true, permission_count: 10 },
];

beforeEach(() => {
  vi.resetAllMocks();
  window.history.replaceState(null, "", paths.users);
  auth.me = me(["user.manage", "role.manage"]);
  auth.refresh = vi.fn(async () => {});
  vi.mocked(identityApi.listUsers).mockResolvedValue(userPage([]));
  vi.mocked(identityApi.listRoles).mockResolvedValue(roles);
});
afterEach(cleanup);

describe("用户与角色管理", () => {
  it("停用前确认，取消不写入；确认携带 UUID 和列表版本", async () => {
    vi.mocked(identityApi.listUsers).mockResolvedValue(userPage([user()]));
    vi.mocked(identityApi.changeUserStatus).mockResolvedValue({ id: "u-author", status: "disabled", version: 8 });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "停用 author" }));
    fireEvent.click(await screen.findByRole("button", { name: /取\s*消/ }));
    expect(identityApi.changeUserStatus).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "停用 author" }));
    vi.mocked(identityApi.listUsers).mockResolvedValue(userPage([user({ status: "disabled", version: 8, can_login: false })]));
    fireEvent.click(await screen.findByRole("button", { name: "确认停用" }));
    await screen.findByText(/已停用 author/);
    expect(identityApi.changeUserStatus).toHaveBeenCalledWith("u-author", "disabled", 7);
    await screen.findByRole("button", { name: "启用 author" });
    expect(screen.getByText("本地密码")).toBeTruthy();
    expect(screen.getByText("账号未启用")).toBeTruthy();
  });

  it("启用账号需要新版本，自己的停用成功后刷新认证状态", async () => {
    vi.mocked(identityApi.listUsers).mockResolvedValue(userPage([user({ status: "disabled", version: 8, can_login: false })]));
    vi.mocked(identityApi.changeUserStatus).mockResolvedValue({ id: "u-author", status: "active", version: 9 });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "启用 author" }));
    vi.mocked(identityApi.listUsers).mockResolvedValue(userPage([user({ id: "u-me", username: "me", version: 9 })]));
    fireEvent.click(await screen.findByRole("button", { name: "确认启用" }));
    await screen.findByText(/已启用 author/);
    expect(identityApi.changeUserStatus).toHaveBeenCalledWith("u-author", "active", 8);
    fireEvent.click(await screen.findByRole("button", { name: "停用 me" }));
    await screen.findByText(/你将退出登录/);
    fireEvent.click(screen.getByRole("button", { name: "确认停用" }));
    await waitFor(() => expect(auth.refresh).toHaveBeenCalledTimes(1));
  });

  it("状态版本冲突不重试写入，刷新列表供核对", async () => {
    vi.mocked(identityApi.listUsers).mockResolvedValue(userPage([user()]));
    vi.mocked(identityApi.changeUserStatus).mockRejectedValue(new ApiError(409, "冲突", "version_conflict"));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "停用 author" }));
    vi.mocked(identityApi.listUsers).mockResolvedValue(userPage([user({ version: 10, display_name: "其他人更新" })]));
    fireEvent.click(await screen.findByRole("button", { name: "确认停用" }));
    await screen.findByText(/请核对最新列表后重试/);
    await screen.findByText("其他人更新");
    expect(identityApi.changeUserStatus).toHaveBeenCalledTimes(1);
  });

  it("只持有 role.manage 看得到状态，但没有启停入口", async () => {
    auth.me = me(["role.manage"]);
    vi.mocked(identityApi.listUsers).mockResolvedValue(userPage([user()]));
    render(<App />);
    await screen.findByText("已启用");
    expect(screen.queryByRole("button", { name: "停用 author" })).toBeNull();
  });

  it("最后 Admin、无所有权权限及已删除账号禁用状态操作", async () => {
    auth.me = me(["user.manage", "admin.manage"]);
    vi.mocked(identityApi.listUsers).mockResolvedValue(userPage([
      user({ username: "last", is_last_loginable_admin: true, roles: ["admin"] }),
      user({ id: "deleted", username: "deleted", deleted: true }),
    ]));
    const view = render(<App />);
    expect((await screen.findByRole("button", { name: "停用 last" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "停用 deleted" }) as HTMLButtonElement).disabled).toBe(true);
    auth.me = me(["user.manage"]);
    view.rerender(<App />);
    expect((screen.getByRole("button", { name: "停用 last" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("没有账号管理权限时不渲染控件，也不调用账号接口", async () => {
    auth.me = me(["post.read"]);
    render(<App />);

    // 屏幕按路由懒加载：等真实屏幕挂载后再断言，避免撞上 Suspense 占位。
    expect(await screen.findByText(/无法查看或管理账号/)).toBeTruthy();
    expect(vi.mocked(identityApi.listUsers)).not.toHaveBeenCalled();
    expect(vi.mocked(identityApi.listRoles)).not.toHaveBeenCalled();
  });

  it("用户名冲突按 username_taken 定位到用户名文案", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByLabelText("用户名")).toBeTruthy());

    vi.mocked(identityApi.createUser).mockRejectedValue(
      new ApiError(409, "username 已被占用", "username_taken", "req-1"),
    );
    fireEvent.change(screen.getByLabelText("用户名"), { target: { value: "author" } });
    fireEvent.click(screen.getByRole("button", { name: "创建账号" }));

    await waitFor(() => expect(screen.getByText("用户名已被占用，请换一个。")).toBeTruthy());
  });

  it("邮箱冲突按 email_taken 给出邮箱文案", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByLabelText("用户名")).toBeTruthy());

    vi.mocked(identityApi.createUser).mockRejectedValue(
      new ApiError(409, "email 已被占用", "email_taken", "req-2"),
    );
    fireEvent.change(screen.getByLabelText("用户名"), { target: { value: "newcomer" } });
    fireEvent.change(screen.getByLabelText("邮箱（可选）"), {
      target: { value: "used@example.com" },
    });
    fireEvent.click(screen.getByRole("button", { name: "创建账号" }));

    await waitFor(() =>
      expect(screen.getByText(/邮箱已被其他账号使用/)).toBeTruthy(),
    );
  });

  it("后端标记为最后一个可登录 Admin 时禁用移除并给出解释", async () => {
    const owner = user({
      id: "u-owner",
      username: "owner",
      roles: ["admin"],
      is_last_loginable_admin: true,
    });
    vi.mocked(identityApi.listUsers).mockResolvedValue(userPage([owner]));
    render(<App />);

    const remove = await screen.findByRole("button", { name: "移除 owner 的角色 admin" });
    expect((remove as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText(/最后 Admin/)).toBeTruthy();
  });

  it("分页可达后续账号，移除另一页 Admin 后刷新全部分页的最后 Admin 状态", async () => {
    auth.me = me(["role.manage", "admin.manage"]);
    const rows = [
      user({ id: "u-owner", username: "owner-a", roles: ["admin"] }),
      ...Array.from({ length: 49 }, (_, index) => user({
        id: `u-${index}`, username: `user-${String(index).padStart(2, "0")}`, roles: [],
      })),
      user({ id: "u-other", username: "zz-owner", roles: ["admin"] }),
    ];
    vi.mocked(identityApi.listUsers).mockImplementation(async (page = 1, perPage = 50) => userPage(rows.slice((page - 1) * perPage, page * perPage), rows.length, page, perPage));
    vi.mocked(identityApi.removeRole).mockImplementation(async () => {
      rows[0] = { ...rows[0], is_last_loginable_admin: true };
      rows[50] = { ...rows[50], roles: [] };
      return null;
    });
    render(<App />);

    const firstAdmin = await screen.findByRole("button", { name: "移除 owner-a 的角色 admin" });
    expect((firstAdmin as HTMLButtonElement).disabled).toBe(false);
    expect(screen.queryByText("zz-owner")).toBeNull();
    expect(identityApi.listUsers).toHaveBeenLastCalledWith(1, 50);
    fireEvent.click(screen.getByTitle("下一页"));
    const remove = await screen.findByRole("button", { name: "移除 zz-owner 的角色 admin" });
    expect(identityApi.listUsers).toHaveBeenLastCalledWith(2, 50);
    expect(screen.getByText("第 2 页")).toBeTruthy();
    expect(screen.getByText("共 51 个账号")).toBeTruthy();
    expect(screen.getByTitle("下一页").getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(remove);
    await waitFor(() => expect(screen.queryByRole("button", { name: "移除 zz-owner 的角色 admin" })).toBeNull());
    expect(identityApi.removeRole).toHaveBeenCalledWith("zz-owner", "admin");
    await waitFor(() => expect(screen.getByTitle("上一页").getAttribute("aria-disabled")).toBe("false"));
    fireEvent.click(screen.getByTitle("上一页"));
    await screen.findByText("（最后 Admin）");
    expect(identityApi.listUsers).toHaveBeenLastCalledWith(1, 50);
    expect((screen.getByRole("button", { name: "移除 owner-a 的角色 admin" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("显示总数并能直接跳到第三页", async () => {
    vi.mocked(identityApi.listUsers).mockImplementation(async (page = 1, perPage = 50) =>
      userPage([user({ id: `u-page-${page}`, username: `page-${page}` })], 101, page, perPage));
    render(<App />);
    await screen.findByText("共 101 个账号");
    fireEvent.click(screen.getByTitle("3"));
    await screen.findByText("page-3");
    expect(screen.getByText("第 3 页")).toBeTruthy();
    expect(identityApi.listUsers).toHaveBeenLastCalledWith(3, 50);
  });

  it("后端 last_admin 竞态显示专属文案并刷新权威状态", async () => {
    vi.mocked(identityApi.listUsers).mockResolvedValue(userPage([
      user({ id: "u-owner", username: "owner", roles: ["admin"] }),
    ]));
    render(<App />);

    const remove = await screen.findByRole("button", { name: "移除 owner 的角色 admin" });
    vi.mocked(identityApi.removeRole).mockRejectedValue(
      new ApiError(403, "不能移除最后一个可登录的 Admin", "last_admin", "req-3"),
    );
    vi.mocked(identityApi.listUsers).mockResolvedValue(userPage([
      user({ id: "u-owner", username: "owner", roles: ["admin"], is_last_loginable_admin: true }),
    ]));
    fireEvent.click(remove);
    await waitFor(() =>
      expect(screen.getByText(/这是最后一个可登录的 Admin/)).toBeTruthy(),
    );
    await screen.findByText("（最后 Admin）");
    expect((screen.getByRole("button", { name: "移除 owner 的角色 admin" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("账号列表加载失败后可原地重试", async () => {
    vi.mocked(identityApi.listUsers).mockRejectedValueOnce(new Error("连接中断"));
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "重试账号列表" }));
    await waitFor(() => expect(screen.queryByText("连接中断")).toBeNull());
    expect(screen.getByText("还没有账号。")).toBeTruthy();
    expect(identityApi.listUsers).toHaveBeenLastCalledWith(1, 50);
  });

  it("角色目录加载失败时显示错误并可重试，而不是伪装成空列表", async () => {
    vi.mocked(identityApi.listUsers).mockResolvedValue(userPage([user()]));
    // 用非 5xx：Query 策略只对 5xx 自动重试，500 会把错误提示推迟到重试耗尽
    // 之后（约 3 秒）。这条用例测的是「失败可见 + 可重试」，不是重试时序。
    vi.mocked(identityApi.listRoles).mockRejectedValueOnce(
      new ApiError(403, "无权查看角色目录", "forbidden", "req-roles"),
    );
    render(<App />);

    await waitFor(() => expect(screen.getByText(/角色目录加载失败/)).toBeTruthy());
    expect(screen.getByText("角色目录不可用")).toBeTruthy();
    // 不能把「加载失败」显示成「暂无可分配角色」，否则一次网络故障会阻断角色分配。
    expect(screen.queryByText("暂无可分配角色")).toBeNull();

    // 重试成功后恢复分配控件。
    vi.mocked(identityApi.listRoles).mockResolvedValue(roles);
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(screen.getByLabelText("为 author 选择角色")).toBeTruthy());
  });

  it("越过委派上限的 forbidden 提示包含请求编号", async () => {
    vi.mocked(identityApi.listUsers).mockResolvedValue(userPage([user()]));
    render(<App />);

    const select = await screen.findByLabelText("为 author 选择角色");
    // antd Select 不是原生 select：`fireEvent.change` 改不动它的值，
    // 要先 mousedown 打开下拉，再点中具体选项（选项带 title=标签文本）。
    fireEvent.mouseDown(select);
    fireEvent.click(await screen.findByTitle("Editor（editor）"));
    const add = screen.getByRole("button", { name: "为 author 添加角色" }) as HTMLButtonElement;
    await waitFor(() => expect(add.disabled).toBe(false));

    vi.mocked(identityApi.assignRole).mockRejectedValue(
      new ApiError(403, "无权执行该操作", "forbidden", "req-4"),
    );
    fireEvent.click(add);

    await waitFor(() => expect(screen.getByText(/没有权限执行该操作/)).toBeTruthy());
    expect(screen.getByText(/req-4/)).toBeTruthy();
  });

  it("改自己的角色成功后主动刷新权限，保持登录", async () => {
    auth.me = me(["user.manage", "role.manage"], "u-me");
    vi.mocked(identityApi.listUsers).mockResolvedValue(userPage([
      user({ id: "u-me", username: "me", roles: ["editor"] }),
    ]));
    vi.mocked(identityApi.removeRole).mockResolvedValue(null);
    render(<App />);

    const remove = await screen.findByRole("button", { name: "移除 me 的角色 editor" });
    await act(async () => {
      fireEvent.click(remove);
    });

    // 目标是自己：版本已递增，界面必须重新读 `/me` 而不是继续显示旧权限。
    expect(auth.refresh).toHaveBeenCalledTimes(1);
  });
});
