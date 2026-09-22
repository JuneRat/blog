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
    seriesApi: { list: vi.fn(), create: vi.fn(), update: vi.fn(), remove: vi.fn(), reorder: vi.fn() },
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
