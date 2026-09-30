// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { identityApi } from "../src/api/identity";
import { ApiError } from "../src/api/client";
import { mediaApi } from "../src/api/media";
import { AdminLayout } from "../src/components/AdminLayout";
import { AdminProviders } from "../src/providers";
import { MediaLibraryScreen } from "../src/screens/MediaLibraryScreen";
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

vi.mock("../src/api/identity", async (load) => {
  const original = await load<typeof import("../src/api/identity")>();
  return { ...original, identityApi: { ...original.identityApi, setOwnAvatar: vi.fn() } };
});
vi.mock("../src/api/media", async (load) => {
  const original = await load<typeof import("../src/api/media")>();
  return { ...original, mediaApi: { ...original.mediaApi, list: vi.fn(), upload: vi.fn() } };
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
    bio: null,
    version: 3,
    avatar_media_id: avatarMediaId,
    avatar_url: avatarMediaId === null ? null : `/media/${avatarMediaId}`,
  };
}

function me(avatarMediaId: string | null) {
  return {
    user_id: "u1",
    username: "author",
    display_name: "作者",
    bio: null,
    version: 3,
    avatar_media_id: avatarMediaId,
    avatar_url: avatarMediaId === null ? null : `/media/${avatarMediaId}`,
    permissions: ["media.read", "media.upload"],
    csrf_token: "csrf",
    channel: "session",
  };
}

function renderLayout(showMedia = false) {
  return render(
    <AdminProviders>
      <UnsavedChangesProvider>
        <AdminLayout>
          {showMedia ? <MediaLibraryScreen /> : <div>内容</div>}
        </AdminLayout>
      </UnsavedChangesProvider>
    </AdminProviders>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  h.refresh.mockReset();
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
  it("选择头像后保存：刷新 /me 和已展示的媒体引用计数", async () => {
    h.me = me(null);
    let saved = false;
    vi.mocked(identityApi.setOwnAvatar).mockImplementation(async () => { saved = true; return profile("m1"); });
    vi.mocked(mediaApi.list).mockImplementation(async () => ({
      items: [{ ...asset, reference_count: saved ? 1 : 0 }], total: 1, page: 1, per_page: 24,
    }));

    renderLayout(true);
    await screen.findByRole("button", { name: "查看使用位置" });
    fireEvent.click(screen.getByRole("button", { name: /更换头像/ }));
    // 外层弹窗里的选择器先打开媒体库弹窗，再从网格里选第一张。
    fireEvent.click(await screen.findByRole("button", { name: "选择封面" }));
    // 锚定匹配：只匹配资产卡上的「选择」，不会命中「选择封面」。
    fireEvent.click(await screen.findByRole("button", { name: /^选\s*择$/ }));
    fireEvent.click(await screen.findByRole("button", { name: /保\s*存/ }));

    await waitFor(() => expect(identityApi.setOwnAvatar).toHaveBeenCalledWith("m1", 3));
    await waitFor(() => expect(h.refresh).toHaveBeenCalled());
    await screen.findByText("被 1 处引用");
  });

  it("移除头像：保存时提交 null", async () => {
    h.me = me("m1");
    vi.mocked(identityApi.setOwnAvatar).mockResolvedValue(profile(null));

    renderLayout();
    fireEvent.click(screen.getByRole("button", { name: /更换头像/ }));
    fireEvent.click(await screen.findByRole("button", { name: "移除封面" }));
    fireEvent.click(await screen.findByRole("button", { name: /保\s*存/ }));

    await waitFor(() => expect(identityApi.setOwnAvatar).toHaveBeenCalledWith(null, 3));
  });

  it("保存失败时在弹窗内展示服务端文案，不刷新 /me", async () => {
    h.me = me(null);
    vi.mocked(identityApi.setOwnAvatar).mockRejectedValue(new Error("头像保存失败"));

    renderLayout();
    fireEvent.click(screen.getByRole("button", { name: /更换头像/ }));
    fireEvent.click(await screen.findByRole("button", { name: "选择封面" }));
    // 锚定匹配：只匹配资产卡上的「选择」，不会命中「选择封面」。
    fireEvent.click(await screen.findByRole("button", { name: /^选\s*择$/ }));
    fireEvent.click(await screen.findByRole("button", { name: /保\s*存/ }));

    await waitFor(() => expect(screen.getByText("头像保存失败")).toBeTruthy());
    expect(h.refresh).not.toHaveBeenCalled();
  });

  it("打开时锁定版本；冲突保留选择，重新打开后才使用刷新版本", async () => {
    h.me = me(null);
    vi.mocked(identityApi.setOwnAvatar).mockRejectedValueOnce(new ApiError(409, "版本冲突", "version_conflict", "req-avatar"));
    h.refresh.mockImplementation(async () => { h.me = { ...me(null), version: 4 }; });
    renderLayout();
    fireEvent.click(screen.getByRole("button", { name: /更换头像/ }));
    // 模拟后台刷新：打开的编辑窗口仍须提交原始版本。
    h.me = { ...me(null), version: 4 };
    fireEvent.click(await screen.findByRole("button", { name: "选择封面" }));
    fireEvent.click(await screen.findByRole("button", { name: /^选\s*择$/ }));
    fireEvent.click(await screen.findByRole("button", { name: /保\s*存/ }));
    await screen.findByText(/请关闭窗口后重新选择头像/);
    expect(identityApi.setOwnAvatar).toHaveBeenCalledWith("m1", 3);
    expect((screen.getByRole("button", { name: /保\s*存/ }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByRole("button", { name: "移除封面" })).toBeTruthy();
    expect(screen.getByText(/req-avatar/)).toBeTruthy();
    expect(identityApi.setOwnAvatar).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole("button", { name: /取\s*消/ }));
    vi.mocked(identityApi.setOwnAvatar).mockResolvedValueOnce(profile(null));
    fireEvent.click(screen.getByRole("button", { name: /更换头像/ }));
    fireEvent.click(await screen.findByRole("button", { name: /保\s*存/ }));
    await waitFor(() => expect(identityApi.setOwnAvatar).toHaveBeenLastCalledWith(null, 4));
  });

});
