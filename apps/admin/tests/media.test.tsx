// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError, mediaApi } from "../src/api";
import { navigate, paths } from "../src/router";
import type { MediaAsset, MediaPage } from "../src/types";

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    status: "authenticated",
    me: { user_id: "me", permissions: ["media.read", "media.upload", "media.delete"] },
  }),
}));

vi.mock("../src/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("../src/api")>();
  return {
    ...original,
    mediaApi: {
      list: vi.fn(),
      detail: vi.fn(),
      upload: vi.fn(),
      remove: vi.fn(),
    },
  };
});

function asset(overrides: Partial<MediaAsset> = {}): MediaAsset {
  return {
    id: "media-1",
    original_name: "photo.png",
    mime: "image/png",
    byte_size: 2048,
    width: 800,
    height: 600,
    status: "ready",
    version: 2,
    created_at: "2026-09-23T10:00:00Z",
    owner_id: "me",
    owner_display: "sun",
    url: "/media/media-1",
    reference_count: 0,
    public_reference_count: 0,
    ...overrides,
  };
}

function pageOf(items: MediaAsset[], total = items.length): MediaPage {
  return { items, total, page: 1, per_page: 24 };
}

beforeEach(() => {
  vi.resetAllMocks();
  window.history.replaceState(null, "", paths.media);
  vi.mocked(mediaApi.list).mockResolvedValue(pageOf([asset()]));
});
afterEach(cleanup);

describe("媒体库屏", () => {
  it("按列表展示文件名、大小、上传者与引用状态", async () => {
    vi.mocked(mediaApi.list).mockResolvedValue(
      pageOf([
        asset(),
        asset({
          id: "media-2",
          original_name: "used.png",
          reference_count: 2,
          public_reference_count: 1,
          owner_id: "someone-else",
        }),
      ]),
    );
    render(<App />);

    await waitFor(() => expect(screen.getByText("photo.png")).toBeTruthy());
    // 两张图的尺寸相同，因此用 getAllByText 断言两者都渲染了尺寸信息。
    expect(screen.getAllByText(/800×600/).length).toBe(2);
    expect(screen.getAllByText(/2\.0 KiB/).length).toBe(2);
    expect(screen.getByText("未被引用（不公开）")).toBeTruthy();
    expect(screen.getByText("被 2 处引用（1 处公开可读）")).toBeTruthy();

    // 未被引用且是本人上传：可删除。
    const deleteButtons = screen.getAllByRole("button", { name: "删除" });
    expect((deleteButtons[0] as HTMLButtonElement).disabled).toBe(false);
    // 仍被引用：按钮禁用，避免必然失败的请求。
    expect((deleteButtons[1] as HTMLButtonElement).disabled).toBe(true);
  });

  it("删除被拒绝时把使用位置摊开", async () => {
    // 列表显示「未被引用」因此按钮可用；删除瞬间另一处内容引用了它，
    // 后端按引用保护拒绝（409 media_in_use）——这正是需要展示使用位置的场景。
    const used = asset();
    vi.mocked(mediaApi.list).mockResolvedValue(pageOf([used]));
    vi.mocked(mediaApi.remove).mockRejectedValue(
      new ApiError(409, "图片仍被 1 处内容引用，先移除引用再删除", "media_in_use"),
    );
    vi.mocked(mediaApi.detail).mockResolvedValue({
      media: used,
      references: [
        {
          kind: "post",
          content_id: "post-id",
          slug: "with-image",
          title: "带图文章",
          status: "published",
          visibility: "public",
          deleted: false,
          public: true,
        },
      ],
      // 引用计数是全局的，但列表按权限过滤；差额必须如实展示。
      hidden_references: 2,
    });
    vi.spyOn(window, "confirm").mockReturnValue(true);

    render(<App />);
    await waitFor(() => expect(screen.getByText("photo.png")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    await waitFor(() => expect(mediaApi.detail).toHaveBeenCalledWith("media-1"));
    expect(screen.getByText(/仍被以下内容引用/)).toBeTruthy();
    expect(screen.getByText(/带图文章/)).toBeTruthy();
    expect(screen.getByText(/已发布；公开可读/)).toBeTruthy();
    // 被权限过滤掉的引用要解释清楚，否则「被 N 处引用」与列表条数会对不上。
    expect(screen.getByText(/另有 2 处引用你无权查看/)).toBeTruthy();
  });

  it("没有隐藏引用时不显示解释文案", async () => {
    // 列表显示「未被引用」因此按钮可用；删除时才发现唯一那处引用（本页可见）。
    const used = asset();
    vi.mocked(mediaApi.list).mockResolvedValue(pageOf([used]));
    vi.mocked(mediaApi.remove).mockRejectedValue(
      new ApiError(409, "图片仍被 1 处内容引用，先移除引用再删除", "media_in_use"),
    );
    vi.mocked(mediaApi.detail).mockResolvedValue({
      media: used,
      references: [
        {
          kind: "post",
          content_id: "post-id",
          slug: "mine",
          title: "我的草稿",
          status: "draft",
          visibility: "public",
          deleted: false,
          public: false,
        },
      ],
      hidden_references: 0,
    });
    vi.spyOn(window, "confirm").mockReturnValue(true);

    render(<App />);
    await waitFor(() => expect(screen.getByText("photo.png")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    await waitFor(() => expect(mediaApi.detail).toHaveBeenCalled());
    expect(screen.queryByText(/无权查看/)).toBeNull();
    expect(screen.getByText(/我的草稿/)).toBeTruthy();
    expect(screen.getByText(/草稿；不公开/)).toBeTruthy();
  });

  it("上传成功后刷新列表", async () => {
    vi.mocked(mediaApi.upload).mockResolvedValue(asset({ id: "new-media" }));
    render(<App />);
    await waitFor(() => expect(screen.getByText("photo.png")).toBeTruthy());

    const input = document.querySelector<HTMLInputElement>('input[type="file"]')!;
    const file = new File([new Uint8Array(16)], "new.png", { type: "image/png" });
    fireEvent.change(input, { target: { files: [file] } });

    await waitFor(() => expect(mediaApi.upload).toHaveBeenCalledWith(file));
    await waitFor(() => expect(mediaApi.list).toHaveBeenCalledTimes(2));
  });

  it("客户端预筛拦下不支持的格式，不发请求", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByText("photo.png")).toBeTruthy());

    const input = document.querySelector<HTMLInputElement>('input[type="file"]')!;
    const svg = new File([new Uint8Array(16)], "evil.svg", { type: "image/svg+xml" });
    fireEvent.change(input, { target: { files: [svg] } });

    await waitFor(() => expect(screen.getByText(/不支持的图片类型/)).toBeTruthy());
    expect(mediaApi.upload).not.toHaveBeenCalled();
  });

  it("分页信息与翻页", async () => {
    vi.mocked(mediaApi.list).mockResolvedValue({
      items: [asset()],
      total: 50,
      page: 1,
      per_page: 24,
    });
    render(<App />);
    await waitFor(() => expect(screen.getByText("第 1 / 3 页")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "下一页" }));
    await waitFor(() => expect(mediaApi.list).toHaveBeenCalledWith(2));
  });

  it("路由与地址对齐：/admin/media 打开媒体库", async () => {
    navigate(paths.media);
    render(<App />);
    await waitFor(() => expect(screen.getByRole("heading", { name: "媒体库" })).toBeTruthy());
  });
});
