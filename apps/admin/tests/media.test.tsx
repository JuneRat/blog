// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { mediaApi } from "../src/api";
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
      restore: vi.fn(),
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
    deleted_at: null,
    version: 2,
    created_at: "2026-09-23T10:00:00Z",
    owner_id: "me",
    owner_display: "sun",
    url: "/media/media-1",
    reference_count: 0,
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
          owner_id: "someone-else",
        }),
      ]),
    );
    render(<App />);

    await waitFor(() => expect(screen.getByText("photo.png")).toBeTruthy());
    // 两张图的尺寸相同，因此用 getAllByText 断言两者都渲染了尺寸信息。
    expect(screen.getAllByText(/800×600/).length).toBe(2);
    expect(screen.getAllByText(/2\.0 KiB/).length).toBe(2);
    expect(screen.getByText(/图片链接独立公开/)).toBeTruthy();
    expect(screen.getByText("被 2 处引用")).toBeTruthy();

    // 未被引用且是本人上传：可删除。
    const deleteButtons = screen.getAllByRole("button", { name: "移入回收站" });
    expect((deleteButtons[0] as HTMLButtonElement).disabled).toBe(false);
    // 其他上传者：没有 media.delete_any 时不能管理。
    expect((deleteButtons[1] as HTMLButtonElement).disabled).toBe(true);
  });

  it("主动查看使用位置并解释隐藏引用", async () => {
    // 使用位置来自独立详情请求，允许隐去没有阅读权限的来源。
    const used = asset();
    vi.mocked(mediaApi.list).mockResolvedValue(pageOf([used]));
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

    render(<App />);
    await waitFor(() => expect(screen.getByText("photo.png")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "查看使用位置" }));
    await waitFor(() => expect(mediaApi.detail).toHaveBeenCalledWith("media-1"));
    expect(await screen.findByText(/的使用位置/)).toBeTruthy();
    expect(screen.getByText(/带图文章/)).toBeTruthy();
    expect(screen.getByText(/已发布；公开可读/)).toBeTruthy();
    // 被权限过滤掉的引用要解释清楚，否则「被 N 处引用」与列表条数会对不上。
    expect(screen.getByText(/另有 2 处引用你无权查看/)).toBeTruthy();
  });

  it("没有隐藏引用时不显示解释文案", async () => {
    // 唯一那处引用对当前用户可见。
    const used = asset();
    vi.mocked(mediaApi.list).mockResolvedValue(pageOf([used]));
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

    render(<App />);
    await waitFor(() => expect(screen.getByText("photo.png")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "查看使用位置" }));
    // 先等使用位置面板出现（唯一那处引用可见），再断言「无隐藏引用」的解释文案缺席。
    expect(await screen.findByText(/我的草稿/)).toBeTruthy();
    expect(screen.queryByText(/无权查看/)).toBeNull();
    expect(screen.getByText(/草稿；不公开/)).toBeTruthy();
  });

  it("被引用的图片也可移入回收站并恢复，两个列表同步更新", async () => {
    let trashed = false;
    const used = asset({ reference_count: 3 });
    vi.mocked(mediaApi.list).mockImplementation(async (_page, trash) =>
      pageOf(Boolean(trash) === trashed ? [{ ...used, deleted_at: trashed ? "2026-09-26T00:00:00Z" : null, version: trashed ? 3 : 2 }] : []),
    );
    vi.mocked(mediaApi.remove).mockImplementation(async () => { trashed = true; });
    vi.mocked(mediaApi.restore).mockImplementation(async () => { trashed = false; });
    render(<App />);
    await screen.findByText("photo.png");
    const remove = screen.getByRole("button", { name: "移入回收站" }) as HTMLButtonElement;
    expect(remove.disabled).toBe(false);
    fireEvent.click(remove);
    expect(await screen.findByText(/图片链接仍公开可访问/)).toBeTruthy();
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));
    await screen.findByText("媒体库还是空的。");
    expect(mediaApi.remove).toHaveBeenCalledWith("media-1", 2);
    fireEvent.click(screen.getByRole("radio", { name: "回收站" }));
    await screen.findByText("photo.png");
    fireEvent.click(screen.getByRole("button", { name: "恢复" }));
    await screen.findByText("回收站是空的。");
    expect(mediaApi.restore).toHaveBeenCalledWith("media-1", 3);
    fireEvent.click(screen.getByRole("radio", { name: "全部图片" }));
    expect(await screen.findByText("photo.png")).toBeTruthy();
  });

  it("上传成功后让列表重取并展示新资产", async () => {
    vi.mocked(mediaApi.upload).mockResolvedValue(asset({ id: "new-media" }));
    // 上传成功后重取的列表里带上新资产：用界面变化证明「刷新了」，
    // 而不是去数 api.list 调用了几次（那是取数实现的细节）。
    vi.mocked(mediaApi.list)
      .mockResolvedValueOnce(pageOf([asset()]))
      .mockResolvedValue(pageOf([asset({ id: "new-media", original_name: "new.png" })]));

    render(<App />);
    await waitFor(() => expect(screen.getByText("photo.png")).toBeTruthy());

    const input = document.querySelector<HTMLInputElement>('input[type="file"]')!;
    const file = new File([new Uint8Array(16)], "new.png", { type: "image/png" });
    fireEvent.change(input, { target: { files: [file] } });

    expect(await screen.findByText("new.png")).toBeTruthy();
    expect(screen.getByText(/已上传 1 张图片/)).toBeTruthy();
    expect(mediaApi.upload).toHaveBeenCalledWith(file);
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

  it("批量上传部分成功后失败，媒体库仍能看到已落库的图片", async () => {
    let uploaded = false;
    const first = asset({ id: "first", original_name: "first.png" });
    vi.mocked(mediaApi.list).mockImplementation(async () => pageOf(uploaded ? [first, asset()] : [asset()]));
    vi.mocked(mediaApi.upload).mockImplementationOnce(async () => { uploaded = true; return first; })
      .mockRejectedValueOnce(new Error("第二张上传失败"));
    render(<App />);
    await screen.findByText("photo.png");
    fireEvent.change(document.querySelector<HTMLInputElement>('input[type="file"]')!, { target: { files: [
      new File(["png"], "first.png", { type: "image/png" }),
      new File(["png"], "second.png", { type: "image/png" }),
    ] } });
    await screen.findByText("第二张上传失败");
    await screen.findByText("first.png");
    expect(screen.queryByText(/已上传 2 张/)).toBeNull();
  });

  it("上传后整族失效：翻回看过的页不展示陈旧缓存", async () => {
    // 上传让全部页内容移位（新资产排在最前）。若只失效第 1 页，
    // 已看过的第 2 页在 30s staleTime 内会展示旧缓存（条目丢失）。
    // mock 按页路由而不是按调用顺序排队：失效重取与 setPage 的先后
    // 存在竞态，顺序队列会让断言偶发错位。
    let uploaded = false;
    vi.mocked(mediaApi.upload).mockImplementation(async () => {
      uploaded = true;
      return asset({ id: "new-media" });
    });
    vi.mocked(mediaApi.list).mockImplementation(async (p: number) => {
      if (p === 1) {
        return uploaded
          ? { items: [asset({ id: "new-media", original_name: "new.png" })], total: 61, page: 1, per_page: 24 }
          : { items: [asset()], total: 60, page: 1, per_page: 24 };
      }
      if (p === 2) {
        return uploaded
          ? { items: [asset({ id: "p2-shifted", original_name: "shifted.png" })], total: 61, page: 2, per_page: 24 }
          : { items: [asset({ id: "p2", original_name: "second-page.png" })], total: 60, page: 2, per_page: 24 };
      }
      return { items: [asset({ id: "p3", original_name: "third-page.png" })], total: 60, page: 3, per_page: 24 };
    });

    render(<App />);
    await waitFor(() => expect(screen.getByText("photo.png")).toBeTruthy());
    fireEvent.click(screen.getByTitle("下一页"));
    await waitFor(() => expect(screen.getByText("second-page.png")).toBeTruthy());
    fireEvent.click(screen.getByTitle("下一页"));
    await waitFor(() => expect(screen.getByText("third-page.png")).toBeTruthy());

    const input = document.querySelector<HTMLInputElement>('input[type="file"]')!;
    const file = new File([new Uint8Array(16)], "new.png", { type: "image/png" });
    fireEvent.change(input, { target: { files: [file] } });
    // 上传成功后自动回到第 1 页并展示新资产。
    await waitFor(() => expect(screen.getByText(/已上传 1 张图片/)).toBeTruthy());
    await waitFor(() => expect(screen.getByText("new.png")).toBeTruthy());

    // 翻回第 2 页：必须看到重取后的移位内容；陈旧缓存会停留在 second-page.png。
    fireEvent.click(screen.getByTitle("下一页"));
    expect(await screen.findByText("shifted.png")).toBeTruthy();
    expect(screen.queryByText("second-page.png")).toBeNull();
    // 第 2 页确实重新请求过（共两次：首次翻页 + 失效后重取）。
    expect(
      mediaApi.list.mock.calls.filter(([p]) => p === 2).length,
    ).toBe(2);
  });

  it("分页信息与翻页", async () => {
    vi.mocked(mediaApi.list).mockResolvedValue({
      items: [asset()],
      total: 50,
      page: 1,
      per_page: 24,
    });
    render(<App />);

    // 第 1 页展示第 1 页数据（请求第 1 页）；总数 50、每页 24 → 共 3 页，当前页为 1。
    await waitFor(() => expect(screen.getByText("photo.png")).toBeTruthy());
    expect(mediaApi.list).toHaveBeenCalledWith(1, false);
    expect(screen.getByText("共 50 张")).toBeTruthy();
    expect(screen.getByTitle("3")).toBeTruthy();
    expect(screen.getByTitle("1").className).toContain("ant-pagination-item-active");

    // antd Pagination 的「下一页」是带 title 的 <li>；点击后请求第 2 页。
    fireEvent.click(screen.getByTitle("下一页"));
    await waitFor(() => expect(mediaApi.list).toHaveBeenCalledWith(2, false));
  });

  it("路由与地址对齐：/admin/media 打开媒体库", async () => {
    navigate(paths.media);
    render(<App />);
    await waitFor(() => expect(screen.getByRole("heading", { name: "媒体库" })).toBeTruthy());
  });

  it("使用位置按引用类型标注：用户/站点设置不再误标为草稿", async () => {
    const used = asset();
    vi.mocked(mediaApi.list).mockResolvedValue(pageOf([used]));
    vi.mocked(mediaApi.detail).mockResolvedValue({
      media: used,
      references: [
        {
          kind: "user",
          content_id: "user-id",
          slug: "author",
          title: "作者甲",
          status: "active",
          visibility: "public",
          deleted: false,
          public: true,
        },
        {
          kind: "site",
          content_id: "00000000-0000-0000-0000-000000000000",
          slug: "",
          title: "站点设置",
          status: "active",
          visibility: "public",
          deleted: false,
          public: true,
        },
      ],
      hidden_references: 0,
    });

    render(<App />);
    await waitFor(() => expect(screen.getByText("photo.png")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "查看使用位置" }));

    expect(await screen.findByText("用户：作者甲")).toBeTruthy();
    expect(screen.getByText("站点设置：站点设置")).toBeTruthy();
    // 两者都是公开来源：只展示公开状态，不再套用「已发布/草稿」。
    expect(screen.getAllByText("公开可读").length).toBe(2);
    expect(screen.queryByText("草稿")).toBeNull();
  });
});
