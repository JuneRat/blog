// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError, api, categoryApi, seriesApi } from "../src/api";
import { navigate, paths } from "../src/router";
import type { SeriesSummary } from "../src/types";

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    status: "authenticated",
    me: { user_id: "me", permissions: ["series.manage", "post.update_any"] },
  }),
}));

vi.mock("../src/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("../src/api")>();
  return {
    ...original,
    seriesApi: { list: vi.fn(), create: vi.fn(), update: vi.fn(), remove: vi.fn(), reorder: vi.fn(), members: vi.fn() },
    categoryApi: { list: vi.fn() },
    api: { ...original.api, listPosts: vi.fn(), listTags: vi.fn() },
  };
});

const guide: SeriesSummary = {
  id: "ser-1", slug: "guide", name: "指南",
  description: null, version: 3, post_count: 2, pub_post_count: 2,
};

const posts = [
  { id: "p1", slug: "part-1", title: "第一篇", status: "published", visibility: "public",
    version: 1, published_at: null, updated_at: "", author_id: "me",
    tag_ids: [], category_id: null, series_id: "ser-1", series_order: 1 },
  { id: "p2", slug: "part-2", title: "第二篇", status: "published", visibility: "public",
    version: 1, published_at: null, updated_at: "", author_id: "me",
    tag_ids: [], category_id: null, series_id: "ser-1", series_order: 2 },
];

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
    expect(screen.getByText(/无权执行该操作/)).toBeTruthy();
  });

  it("创建系列并展示删除保护", async () => {
    vi.mocked(seriesApi.create).mockResolvedValue(guide);
    vi.mocked(seriesApi.remove).mockRejectedValue(
      new ApiError(409, "系列仍被 2 篇文章引用，先解除关联再删除", "series_in_use", "req-2"),
    );
    window.confirm = vi.fn(() => true);
    render(<App />);
    await waitFor(() => expect(screen.getByText("指南")).toBeTruthy());

    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "新系列" } });
    fireEvent.change(screen.getByLabelText("slug"), { target: { value: "new-series" } });
    fireEvent.click(screen.getByRole("button", { name: "创建系列" }));
    await waitFor(() =>
      expect(seriesApi.create).toHaveBeenCalledWith({ name: "新系列", slug: "new-series" }),
    );

    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    await waitFor(() => expect(seriesApi.remove).toHaveBeenCalledWith("guide", 3));
    expect(screen.getByText(/2 篇文章引用/)).toBeTruthy();
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
      { id: "ser-1", slug: "guide", name: "指南", description: null, version: 1, post_count: 0, pub_post_count: 0 },
    ]);
    const apiAny = api as unknown as Record<string, ReturnType<typeof vi.fn>>;
    apiAny.createPost = vi.fn().mockResolvedValue(post);
    apiAny.getPost = vi.fn().mockResolvedValue(post);
    apiAny.updatePost = vi.fn();
  });

  it("选择系列但序号为空：展示错误且不发请求（不静默丢系列）", async () => {
    render(<App />);
    const title = await screen.findByLabelText("标题");
    fireEvent.change(title, { target: { value: "新篇" } });
    fireEvent.change(await screen.findByLabelText("系列"), { target: { value: "ser-1" } });
    // 序号留空。
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));

    expect(
      screen.getByText("选择了系列时，系列内序号必须是正整数（如 1、2、3）。"),
    ).toBeTruthy();
    const apiAny = api as unknown as Record<string, ReturnType<typeof vi.fn>>;
    expect(apiAny.createPost).not.toHaveBeenCalled();
  });

  it("小数序号（1.5）被拒绝，不被 parseInt 截断", async () => {
    render(<App />);
    fireEvent.change(await screen.findByLabelText("标题"), { target: { value: "新篇" } });
    fireEvent.change(await screen.findByLabelText("系列"), { target: { value: "ser-1" } });
    fireEvent.change(screen.getByLabelText("系列内序号"), { target: { value: "1.5" } });
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));

    expect(screen.getByText(/正整数/)).toBeTruthy();
    const apiAny = api as unknown as Record<string, ReturnType<typeof vi.fn>>;
    expect(apiAny.createPost).not.toHaveBeenCalled();
  });

  it("合法序号随载荷提交系列", async () => {
    render(<App />);
    fireEvent.change(await screen.findByLabelText("标题"), { target: { value: "新篇" } });
    fireEvent.change(await screen.findByLabelText("系列"), { target: { value: "ser-1" } });
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
