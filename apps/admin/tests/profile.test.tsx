// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError, api } from "../src/api";
import { paths } from "../src/router";
import type { Me } from "../src/types";

const auth = vi.hoisted(() => ({ me: null as Me | null, refresh: vi.fn() }));
vi.mock("../src/auth", () => ({
  useAuth: () => ({ status: "authenticated", me: auth.me, refresh: auth.refresh }),
}));
vi.mock("../src/api", async (original) => {
  const module = await original<typeof import("../src/api")>();
  return { ...module, api: { ...module.api, me: vi.fn(), updateOwnProfile: vi.fn(), listUsers: vi.fn() } };
});

const profile: Me = {
  user_id: "u-profile", username: "author", display_name: "原展示名", bio: "原简介", version: 7,
  avatar_media_id: null, avatar_url: null, permissions: [], csrf_token: "csrf", channel: "session",
};

beforeEach(() => {
  vi.resetAllMocks();
  window.history.replaceState(null, "", paths.profile);
  auth.me = profile;
  auth.refresh.mockResolvedValue(undefined);
  vi.mocked(api.me).mockResolvedValue(profile);
});
afterEach(cleanup);

it("任何已登录账号都可编辑资料，使用读取时的版本并保持登录", async () => {
  vi.mocked(api.updateOwnProfile).mockResolvedValue({ ...profile, display_name: "新展示名", bio: null, version: 8 });
  render(<App />);
  await screen.findByDisplayValue("原展示名");
  fireEvent.change(screen.getByLabelText("展示名"), { target: { value: " 新展示名 " } });
  fireEvent.change(screen.getByLabelText("个人简介"), { target: { value: "" } });
  fireEvent.click(screen.getByRole("button", { name: /保存资料$/ }));
  await screen.findByText("个人资料已保存。");
  expect(api.updateOwnProfile).toHaveBeenCalledWith({ display_name: "新展示名", bio: null, expected_version: 7 });
  expect(auth.refresh).toHaveBeenCalledTimes(1);
  expect((screen.getByRole("button", { name: /保存资料$/ }) as HTMLButtonElement).disabled).toBe(true);
  const event = new Event("beforeunload", { cancelable: true });
  window.dispatchEvent(event);
  expect(event.defaultPrevented).toBe(false);
});

it("版本冲突保留输入并停止提交，显式加载后使用新版本", async () => {
  vi.mocked(api.updateOwnProfile).mockRejectedValueOnce(new ApiError(409, "冲突", "version_conflict", "req-profile"));
  render(<App />);
  await screen.findByDisplayValue("原展示名");
  fireEvent.change(screen.getByLabelText("展示名"), { target: { value: "我的输入" } });
  fireEvent.click(screen.getByRole("button", { name: /保存资料$/ }));
  await screen.findByText(/你的输入已保留/);
  expect((screen.getByLabelText("展示名") as HTMLInputElement).value).toBe("我的输入");
  expect((screen.getByRole("button", { name: /保存资料$/ }) as HTMLButtonElement).disabled).toBe(true);
  vi.mocked(api.me).mockResolvedValue({ ...profile, display_name: "别处更新", version: 9 });
  fireEvent.click(screen.getByRole("button", { name: "重新加载并放弃修改" }));
  await screen.findByDisplayValue("别处更新");
  await screen.findByRole("button", { name: /保存资料$/ });
  vi.mocked(api.updateOwnProfile).mockResolvedValue({ ...profile, display_name: "新的输入", version: 10 });
  fireEvent.change(screen.getByLabelText("展示名"), { target: { value: "新的输入" } });
  fireEvent.click(screen.getByRole("button", { name: /保存资料$/ }));
  await waitFor(() => expect(api.updateOwnProfile).toHaveBeenLastCalledWith({ display_name: "新的输入", bio: "原简介", expected_version: 9 }));
});

it("保存成功但账号刷新失败时仍保留提交版本，不把资料当作未保存", async () => {
  auth.refresh.mockRejectedValue(new Error("网络中断"));
  vi.mocked(api.updateOwnProfile).mockResolvedValue({ ...profile, display_name: "已保存", version: 8 });
  render(<App />);
  await screen.findByDisplayValue("原展示名");
  fireEvent.change(screen.getByLabelText("展示名"), { target: { value: "已保存" } });
  fireEvent.click(screen.getByRole("button", { name: /保存资料$/ }));
  await screen.findByText(/资料已保存，账号信息刷新失败/);
  expect((screen.getByRole("button", { name: /保存资料$/ }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("个人简介"), { target: { value: "另一次修改" } });
  fireEvent.click(screen.getByRole("button", { name: /保存资料$/ }));
  await waitFor(() => expect(api.updateOwnProfile).toHaveBeenLastCalledWith({ display_name: "已保存", bio: "另一次修改", expected_version: 8 }));
});

it("修改资料后导航与刷新均提示未保存，取消导航保留输入", async () => {
  render(<App />);
  await screen.findByDisplayValue("原展示名");
  fireEvent.change(screen.getByLabelText("个人简介"), { target: { value: "未保存" } });
  const event = new Event("beforeunload", { cancelable: true });
  window.dispatchEvent(event);
  expect(event.defaultPrevented).toBe(true);
  fireEvent.click(screen.getByRole("menuitem", { name: "用户与角色" }));
  await screen.findByRole("dialog", { name: "有未保存的修改" });
  fireEvent.click(screen.getByRole("button", { name: "留在此页" }));
  await act(async () => {});
  expect(window.location.pathname).toBe(paths.profile);
  expect((screen.getByLabelText("个人简介") as HTMLTextAreaElement).value).toBe("未保存");
  expect(api.updateOwnProfile).not.toHaveBeenCalled();
});

it("网络失败保留表单与原版本，可重试", async () => {
  vi.mocked(api.updateOwnProfile).mockRejectedValueOnce(new Error("保存失败"));
  render(<App />);
  await screen.findByDisplayValue("原展示名");
  fireEvent.change(screen.getByLabelText("展示名"), { target: { value: "保留输入" } });
  fireEvent.click(screen.getByRole("button", { name: /保存资料$/ }));
  await screen.findByText("保存失败");
  expect((screen.getByLabelText("展示名") as HTMLInputElement).value).toBe("保留输入");
  expect(auth.refresh).not.toHaveBeenCalled();
  expect((screen.getByRole("button", { name: /保存资料$/ }) as HTMLButtonElement).disabled).toBe(false);
});
