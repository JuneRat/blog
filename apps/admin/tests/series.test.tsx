// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError, api, categoryApi, mediaApi, seriesApi } from "../src/api";
import { navigate, paths } from "../src/router";
import type { MediaAsset, MediaPage, SeriesSummary } from "../src/types";

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    status: "authenticated",
    me: {
      user_id: "me",
      permissions: ["series.manage", "post.update_any", "media.read", "media.upload"],
    },
  }),
}));

vi.mock("../src/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("../src/api")>();
  return {
    ...original,
    seriesApi: { list: vi.fn(), create: vi.fn(), update: vi.fn(), remove: vi.fn(), reorder: vi.fn(), members: vi.fn() },
    categoryApi: { list: vi.fn() },
    mediaApi: { list: vi.fn(), detail: vi.fn(), upload: vi.fn(), remove: vi.fn() },
    api: { ...original.api, listPosts: vi.fn(), listTags: vi.fn() },
  };
});

const guide: SeriesSummary = {
  id: "ser-1", slug: "guide", name: "指南",
  description: null, version: 3, post_count: 2, pub_post_count: 2,
  cover_media_id: null, cover_url: null,
};

const posts = [
  { id: "p1", slug: "part-1", title: "第一篇", status: "published", visibility: "public",
    version: 1, published_at: null, updated_at: "", author_id: "me",
    tag_ids: [], category_id: null, series_id: "ser-1", series_order: 1,
    cover_media_id: null, cover_url: null },
  { id: "p2", slug: "part-2", title: "第二篇", status: "published", visibility: "public",
    version: 1, published_at: null, updated_at: "", author_id: "me",
    tag_ids: [], category_id: null, series_id: "ser-1", series_order: 2,
    cover_media_id: null, cover_url: null },
];

function asset(overrides: Partial<MediaAsset> = {}): MediaAsset {
  return {
    id: "media-1",
    original_name: "cover.png",
    mime: "image/png",
    byte_size: 2048,
    width: 800,
    height: 600,
    status: "ready",
    version: 1,
    created_at: "2026-09-23T10:00:00Z",
    owner_id: "me",
    owner_display: "sun",
    url: "/media/media-1",
    reference_count: 0,
    public_reference_count: 0,
    ...overrides,
  };
}

function pageOf(items: MediaAsset[]): MediaPage {
  return { items, total: items.length, page: 1, per_page: 24 };
}

beforeEach(() => {
  vi.resetAllMocks();
  window.history.replaceState(null, "", paths.series);
  vi.mocked(seriesApi.list).mockResolvedValue([guide]);
  vi.mocked(seriesApi.members).mockResolvedValue([
    { ...posts[0], visibility: "public" },
    { ...posts[1], visibility: "public" },
  ]);
  vi.mocked(api.listPosts).mockResolvedValue(posts);
  vi.mocked(api.listTags).mockResolvedValue([]);
  vi.mocked(categoryApi.list).mockResolvedValue([]);
  // 封面选择器打开时才取媒体库第一页；默认给一张可选图片。
  vi.mocked(mediaApi.list).mockResolvedValue(pageOf([asset()]));
});
afterEach(cleanup);

describe("系列管理屏", () => {
  it("列出成员并整体重排（交换后提交完整顺序与版本）", async () => {
    vi.mocked(seriesApi.reorder).mockResolvedValue({
      series_version: 4,
      ordered_post_ids: ["p2", "p1"],
    });
    render(<App />);
    await waitFor(() => expect(screen.getByText("第一篇")).toBeTruthy());

    fireEvent.click(screen.getAllByRole("button", { name: "↑" })[1]); // 第二篇上移
    await waitFor(() =>
      expect(seriesApi.reorder).toHaveBeenCalledWith("guide", ["p2", "p1"], 3),
    );
  });

  it("重排越权（403）展示服务端文案并重载", async () => {
    vi.mocked(seriesApi.reorder).mockRejectedValue(
      new ApiError(403, "无权执行该操作", "forbidden", "req-1"),
    );
    render(<App />);
    await waitFor(() => expect(screen.getByText("第一篇")).toBeTruthy());
    fireEvent.click(screen.getAllByRole("button", { name: "↑" })[1]);
    await waitFor(() => expect(seriesApi.reorder).toHaveBeenCalled());
    expect(await screen.findByText(/无权执行该操作/)).toBeTruthy();
  });

  it("创建系列并展示删除保护", async () => {
    vi.mocked(seriesApi.create).mockResolvedValue(guide);
    vi.mocked(seriesApi.remove).mockRejectedValue(
      new ApiError(409, "系列仍被 2 篇文章引用，先解除关联再删除", "series_in_use", "req-2"),
    );
    render(<App />);
    await waitFor(() => expect(screen.getByText("指南")).toBeTruthy());

    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "新系列" } });
    fireEvent.change(screen.getByLabelText("slug"), { target: { value: "new-series" } });
    fireEvent.click(screen.getByRole("button", { name: "创建系列" }));
    await waitFor(() =>
      expect(seriesApi.create).toHaveBeenCalledWith({ name: "新系列", slug: "new-series" }),
    );

    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    // 确认弹窗改由 antd 的 modal.confirm 渲染，必须点掉它才会发请求。
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));
    await waitFor(() => expect(seriesApi.remove).toHaveBeenCalledWith("guide", 3));
    expect(await screen.findByText(/2 篇文章引用/)).toBeTruthy();
  });
});

/**
 * 系列封面：从列表行的「封面」按钮进入弹窗，选择/移除后由「保存」提交。
 *
 * 后端要求系列更新的 `name` 必填，因此改封面也必须原样带上名称与描述，
 * 并携带当前 `version` 作为 expected_version。
 */
describe("系列屏：封面", () => {
  it("选择封面后保存：提交 cover_media_id 与 expected_version，并刷新目录", async () => {
    vi.mocked(seriesApi.update).mockResolvedValue({
      ...guide, cover_media_id: "media-1", cover_url: "/media/media-1", version: 4,
    });
    render(<App />);
    await waitFor(() => expect(screen.getByText("指南")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "封面" }));
    fireEvent.click(await screen.findByRole("button", { name: "选择封面" }));
    // 弹窗打开才拉媒体库第一页。
    await waitFor(() => expect(mediaApi.list).toHaveBeenCalledWith(1));
    fireEvent.click(await screen.findByRole("button", { name: "选择" }));

    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(seriesApi.update).toHaveBeenCalledWith("guide", {
        name: "指南",
        description: undefined,
        cover_media_id: "media-1",
        expected_version: 3,
      }),
    );
    // 保存成功后目录失效重取：初始一次 + 刷新一次。
    await waitFor(() => expect(seriesApi.list).toHaveBeenCalledTimes(2));
    expect(await screen.findByText(/已更新系列 指南 的封面/)).toBeTruthy();
  });

  it("更换封面时预览改用新资产地址，不停留在旧封面", async () => {
    // 目录里的旧封面是 media-9；媒体库第一页给的是 media-1。
    const withCover: SeriesSummary = {
      ...guide, cover_media_id: "media-9", cover_url: "/media/media-9",
    };
    vi.mocked(seriesApi.list).mockResolvedValue([withCover]);
    render(<App />);
    await waitFor(() => expect(screen.getByText("指南")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "封面" }));
    fireEvent.click(await screen.findByRole("button", { name: "更换封面" }));
    fireEvent.click(await screen.findByRole("button", { name: "选择" }));

    // 媒体库弹窗已收起，页面上只剩预览：它必须指向新选中的资产。
    expect(screen.queryByRole("dialog", { name: "选择图片" })).toBeNull();
    expect(document.querySelector('img[src="/media/media-1"]')).toBeTruthy();
  });

  it("移除封面：提交 cover_media_id: null（与「不改封面」区分开）", async () => {
    const withCover: SeriesSummary = {
      ...guide, cover_media_id: "media-9", cover_url: "/media/media-9",
    };
    vi.mocked(seriesApi.list).mockResolvedValue([withCover]);
    vi.mocked(seriesApi.update).mockResolvedValue({
      ...withCover, cover_media_id: null, cover_url: null, version: 4,
    });
    render(<App />);
    await waitFor(() => expect(screen.getByText("指南")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "封面" }));
    fireEvent.click(await screen.findByRole("button", { name: "移除封面" }));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() =>
      expect(seriesApi.update).toHaveBeenCalledWith(
        "guide",
        expect.objectContaining({ cover_media_id: null, expected_version: 3 }),
      ),
    );
  });

  it("版本冲突：展示服务端文案并重新加载目录", async () => {
    vi.mocked(seriesApi.update).mockRejectedValue(
      new ApiError(409, "系列已在别处修改", "version_conflict", "req-3"),
    );
    render(<App />);
    await waitFor(() => expect(screen.getByText("指南")).toBeTruthy());

    fireEvent.click(screen.getByRole("button", { name: "封面" }));
    fireEvent.click(await screen.findByRole("button", { name: "选择封面" }));
    fireEvent.click(await screen.findByRole("button", { name: "选择" }));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    // 内联 Alert 展示服务端原因（含错误编号），并已重读目录。
    expect(await screen.findByText(/系列已在别处修改（错误编号 req-3）/)).toBeTruthy();
    await waitFor(() => expect(seriesApi.list).toHaveBeenCalledTimes(2));
  });
});

describe("文章编辑器系列校验", () => {
  const post = {
    id: "post-id", slug: "ed-1", title: "标题", content: "正文", excerpt: null,
    status: "draft", visibility: "public" as const, version: 2,
    published_at: null, updated_at: "2026-09-22T00:00:00Z", author_id: "me",
    tag_ids: [], category_id: null, series_id: null, series_order: null,
  };

  beforeEach(() => {
    window.history.replaceState(null, "", paths.newPost);
    vi.mocked(api.listTags).mockResolvedValue([]);
    vi.mocked(categoryApi.list).mockResolvedValue([]);
    vi.mocked(seriesApi.list).mockResolvedValue([
      { id: "ser-1", slug: "guide", name: "指南", description: null, version: 1, post_count: 0, pub_post_count: 0, cover_media_id: null, cover_url: null },
    ]);
    const apiAny = api as unknown as Record<string, ReturnType<typeof vi.fn>>;
    apiAny.createPost = vi.fn().mockResolvedValue(post);
    apiAny.getPost = vi.fn().mockResolvedValue(post);
    apiAny.updatePost = vi.fn();
  });

  /**
   * 选中 antd Select 的选项：它没有原生 `<select>`，`fireEvent.change`
   * 只改输入框里的过滤文字、不会产生选中值，必须先在 combobox 上 mouseDown
   * 展开下拉，再点中带 `title` 的选项（与 categories.test.tsx 同一写法）。
   */
  async function selectOption(labelText: string, optionTitle: string): Promise<void> {
    fireEvent.mouseDown(screen.getByLabelText(labelText));
    fireEvent.click(await screen.findByTitle(optionTitle));
  }

  it("选择系列但序号为空：展示错误且不发请求（不静默丢系列）", async () => {
    render(<App />);
    const title = await screen.findByLabelText("标题");
    fireEvent.change(title, { target: { value: "新篇" } });
    await selectOption("系列", "指南");
    // 序号留空。
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));

    // antd Form 的 onFinish 是异步的，校验错误要等文案出现。
    expect(
      await screen.findByText("选择了系列时，系列内序号必须是正整数（如 1、2、3）。"),
    ).toBeTruthy();
    const apiAny = api as unknown as Record<string, ReturnType<typeof vi.fn>>;
    expect(apiAny.createPost).not.toHaveBeenCalled();
  });

  it("小数序号（1.5）被拒绝，不被 parseInt 截断", async () => {
    render(<App />);
    fireEvent.change(await screen.findByLabelText("标题"), { target: { value: "新篇" } });
    await selectOption("系列", "指南");
    fireEvent.change(screen.getByLabelText("系列内序号"), { target: { value: "1.5" } });
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));

    expect(await screen.findByText(/正整数/)).toBeTruthy();
    const apiAny = api as unknown as Record<string, ReturnType<typeof vi.fn>>;
    expect(apiAny.createPost).not.toHaveBeenCalled();
  });

  it("合法序号随载荷提交系列", async () => {
    render(<App />);
    fireEvent.change(await screen.findByLabelText("标题"), { target: { value: "新篇" } });
    await selectOption("系列", "指南");
    fireEvent.change(screen.getByLabelText("系列内序号"), { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));

    const apiAny = api as unknown as Record<string, ReturnType<typeof vi.fn>>;
    await waitFor(() =>
      expect(apiAny.createPost).toHaveBeenCalledWith(
        expect.objectContaining({ series: { id: "ser-1", order: 3 } }),
      ),
    );
  });
});

describe("系列屏：混合系列不可读不连带清空独著系列", () => {
  const solo: SeriesSummary = {
    id: "ser-solo", slug: "solo", name: "独著",
    description: null, version: 1, post_count: 1, pub_post_count: 1,
    cover_media_id: null, cover_url: null,
  };
  const mixed: SeriesSummary = {
    id: "ser-mixed", slug: "mixed", name: "混合",
    description: null, version: 1, post_count: 2, pub_post_count: 2,
    cover_media_id: null, cover_url: null,
  };
  const ownPost = {
    id: "p1", slug: "solo-1", title: "我的独著篇", status: "published",
    visibility: "public", version: 1, published_at: null, updated_at: "",
    author_id: "me", series_order: 1,
  };

  beforeEach(() => {
    window.history.replaceState(null, "", paths.series);
    vi.mocked(seriesApi.list).mockResolvedValue([solo, mixed]);
    // 独著可读；混合（含他人文章，无 read_any）403。
    vi.mocked(seriesApi.members).mockImplementation(async (slug: string) => {
      if (slug === "solo") return [{ ...ownPost }];
      throw new ApiError(403, "无权执行该操作", "forbidden", "req-x");
    });
    vi.mocked(api.listTags).mockResolvedValue([]);
    vi.mocked(categoryApi.list).mockResolvedValue([]);
  });

  it("可读系列照常展示成员，不可读系列显示权限提示而非清空", async () => {
    render(<App />);
    // 独著系列仍然可见且带成员。
    await waitFor(() => expect(screen.getByText("我的独著篇")).toBeTruthy());
    expect(screen.getByText("独著")).toBeTruthy();
    expect(screen.getByText("混合")).toBeTruthy();
    // 混合系列：权限提示，不是「还没有文章加入」的空目录文案。
    expect(screen.getByText(/成员目录不可读/)).toBeTruthy();
    expect(screen.queryByText("还没有文章加入这个系列。")).toBeNull();
    // 独著系列的重排按钮仍在；混合系列的重排按钮（↑/↓）不出现。
    expect(screen.getAllByRole("button", { name: "↑" }).length).toBeGreaterThanOrEqual(1);
  });

  it("目录列表本身失败才清空并报全局错误", async () => {
    vi.mocked(seriesApi.list).mockRejectedValue(
      new ApiError(401, "未登录或会话已失效", "unauthenticated", "req-y"),
    );
    render(<App />);
    await waitFor(() => expect(screen.getByText(/未登录/)).toBeTruthy());
    // 全局失败：目录清空，系列条目不渲染；空态文案只在无错误时出现。
    expect(screen.queryByText("独著")).toBeNull();
    expect(screen.queryByText("混合")).toBeNull();
    expect(screen.queryByText(/成员目录不可读/)).toBeNull();
  });
});
