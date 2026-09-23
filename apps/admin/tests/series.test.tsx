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
  };
  const mixed: SeriesSummary = {
    id: "ser-mixed", slug: "mixed", name: "混合",
    description: null, version: 1, post_count: 2, pub_post_count: 2,
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
