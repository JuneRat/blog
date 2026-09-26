// @vitest-environment jsdom
import { App as AntdApp } from "antd";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api, mediaApi } from "../src/api";
import { AdminLayout } from "../src/components/AdminLayout";
import { UnsavedChangesProvider } from "../src/unsaved";
import type { MediaAsset } from "../src/types";

/**
 * 外壳头像入口：本人自助更换/移除，成功后刷新 `/me` 让头部同步。
 *
 * 认证上下文用 `vi.hoisted` 暴露可变的 `me` 与 `refresh`，避免 mock 工厂
 * 引用尚未初始化的模块级变量（vi.mock 会被提升到 import 之前）。
 */
const h = vi.hoisted(() => ({
  me: {} as Record<string, unknown>,
  refresh: vi.fn(),
}));

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    me: h.me,
    refresh: h.refresh,
    logout: vi.fn(),
    logoutError: null,
  }),
}));

vi.mock("../src/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("../src/api")>();
  return {
    ...original,
    api: { ...original.api, setOwnAvatar: vi.fn() },
    mediaApi: { ...original.mediaApi, list: vi.fn(), upload: vi.fn() },
  };
});

const asset: MediaAsset = {
  id: "m1",
  original_name: "avatar.png",
  mime: "image/png",
  byte_size: 10,
  width: 8,
  height: 8,
  deleted_at: null,
  version: 2,
  created_at: "2026-01-01",
  owner_id: "u1",
  owner_display: "作者",
  url: "/media/m1",
  reference_count: 0,
};

function profile(avatarMediaId: string | null) {
  return {
    user_id: "u1",
    username: "author",
    display_name: "作者",
    avatar_media_id: avatarMediaId,
    avatar_url: avatarMediaId === null ? null : `/media/${avatarMediaId}`,
  };
}

function me(avatarMediaId: string | null) {
  return {
    user_id: "u1",
    username: "author",
    display_name: "作者",
    avatar_media_id: avatarMediaId,
    avatar_url: avatarMediaId === null ? null : `/media/${avatarMediaId}`,
    permissions: ["media.read", "media.upload"],
    csrf_token: "csrf",
    channel: "session",
  };
}

function renderLayout() {
  return render(
    <AntdApp>
      <UnsavedChangesProvider>
        <AdminLayout>
          <div>内容</div>
        </AdminLayout>
      </UnsavedChangesProvider>
    </AntdApp>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  window.history.replaceState(null, "", "/admin/");
  vi.mocked(mediaApi.list).mockResolvedValue({
    items: [asset],
    total: 1,
    page: 1,
    per_page: 24,
  });
});

afterEach(cleanup);

describe("外壳头像入口", () => {
  it("选择头像后保存：调用 setOwnAvatar 并刷新 /me", async () => {
    h.me = me(null);
    vi.mocked(api.setOwnAvatar).mockResolvedValue(profile("m1"));

    renderLayout();
    fireEvent.click(screen.getByRole("button", { name: /更换头像/ }));
    // 外层弹窗里的选择器先打开媒体库弹窗，再从网格里选第一张。
    fireEvent.click(await screen.findByRole("button", { name: "选择封面" }));
    // 锚定匹配：只匹配资产卡上的「选择」，不会命中「选择封面」。
    fireEvent.click(await screen.findByRole("button", { name: /^选\s*择$/ }));
    fireEvent.click(await screen.findByRole("button", { name: /保\s*存/ }));

    await waitFor(() => expect(api.setOwnAvatar).toHaveBeenCalledWith("m1"));
    await waitFor(() => expect(h.refresh).toHaveBeenCalled());
  });

  it("移除头像：保存时提交 null", async () => {
    h.me = me("m1");
    vi.mocked(api.setOwnAvatar).mockResolvedValue(profile(null));

    renderLayout();
    fireEvent.click(screen.getByRole("button", { name: /更换头像/ }));
    fireEvent.click(await screen.findByRole("button", { name: "移除封面" }));
    fireEvent.click(await screen.findByRole("button", { name: /保\s*存/ }));

    await waitFor(() => expect(api.setOwnAvatar).toHaveBeenCalledWith(null));
  });

  it("保存失败时在弹窗内展示服务端文案，不刷新 /me", async () => {
    h.me = me(null);
    vi.mocked(api.setOwnAvatar).mockRejectedValue(new Error("头像保存失败"));

    renderLayout();
    fireEvent.click(screen.getByRole("button", { name: /更换头像/ }));
    fireEvent.click(await screen.findByRole("button", { name: "选择封面" }));
    // 锚定匹配：只匹配资产卡上的「选择」，不会命中「选择封面」。
    fireEvent.click(await screen.findByRole("button", { name: /^选\s*择$/ }));
    fireEvent.click(await screen.findByRole("button", { name: /保\s*存/ }));

    await waitFor(() => expect(screen.getByText("头像保存失败")).toBeTruthy());
    expect(h.refresh).not.toHaveBeenCalled();
  });
});
