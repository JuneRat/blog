// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError, api, seriesApi } from "../src/api";
import { navigate, paths } from "../src/router";
import type { TagSummary } from "../src/types";

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    status: "authenticated",
    me: { permissions: ["tag.manage"] },
  }),
}));

vi.mock("../src/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("../src/api")>();
  return {
    ...original,
    categoryApi: { list: vi.fn() },
    seriesApi: { list: vi.fn() },
    api: {
      listTags: vi.fn(),
      createTag: vi.fn(),
      categoryApi: { list: vi.fn() },
      renameTag: vi.fn(),
      deleteTag: vi.fn(),
    },
  };
});

const rust: TagSummary = {
  id: "tag-rust",
  slug: "rust",
  name: "Rust",
  version: 1,
  public_post_count: 2,
};

const essay: TagSummary = {
  id: "tag-essay",
  slug: "essay",
  name: "随笔",
  version: 1,
  public_post_count: 0,
};

beforeEach(() => {
  vi.resetAllMocks();
  window.history.replaceState(null, "", paths.tags);
  vi.mocked(api.listTags).mockResolvedValue([essay, rust]);
});
afterEach(cleanup);

describe("标签管理屏", () => {
  it("列出目录并创建标签", async () => {
    vi.mocked(api.createTag).mockResolvedValue({ ...rust, id: "new" });
    render(<App />);

    await waitFor(() => expect(screen.getByText("Rust")).toBeTruthy());
    expect(screen.getByText("2")).toBeTruthy(); // 公开文章计数
    expect(screen.getAllByText("v1").length).toBe(2); // 两行目录各带版本

    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "新标签" } });
    fireEvent.change(screen.getByLabelText("slug"), { target: { value: "new-tag" } });
    fireEvent.click(screen.getByRole("button", { name: "创建标签" }));

    await waitFor(() =>
      expect(api.createTag).toHaveBeenCalledWith({ name: "新标签", slug: "new-tag" }),
    );
    // 创建后重载目录。
    await waitFor(() => expect(api.listTags).toHaveBeenCalledTimes(2));
  });

  it("空名称不被提交", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByText("Rust")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "创建标签" }));
    expect(screen.getByText("名称与 slug 都不能为空。")).toBeTruthy();
    expect(api.createTag).not.toHaveBeenCalled();
  });

  it("改名携带当前版本，冲突时展示错误并重载", async () => {
    vi.mocked(api.renameTag).mockRejectedValue(
      new ApiError(409, "版本冲突：内容已被并发修改", "version_conflict", "req-1"),
    );
    render(<App />);
    await waitFor(() => expect(screen.getByText("Rust")).toBeTruthy());

    fireEvent.click(screen.getAllByRole("button", { name: "改名" })[1]); // rust 行
    const input = screen.getByDisplayValue("Rust") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "Rust 语言" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(api.renameTag).toHaveBeenCalledWith("rust", { name: "Rust 语言", expected_version: 1 }));
    expect(screen.getByText(/版本冲突/)).toBeTruthy();
    await waitFor(() => expect(api.listTags).toHaveBeenCalledTimes(2));
  });

  it("删除被引用的标签展示服务端 tag_in_use 文案", async () => {
    vi.mocked(api.deleteTag).mockRejectedValue(
      new ApiError(409, "标签仍被文章引用（3 篇），先解除关联再删除", "tag_in_use", "req-2"),
    );
    window.confirm = vi.fn(() => true);
    render(<App />);
    await waitFor(() => expect(screen.getByText("Rust")).toBeTruthy());

    fireEvent.click(screen.getAllByRole("button", { name: "删除" })[1]);
    await waitFor(() => expect(api.deleteTag).toHaveBeenCalledWith("rust", 1));
    expect(screen.getByText(/3 篇/)).toBeTruthy();
  });

  it("删除确认被取消时不发请求", async () => {
    window.confirm = vi.fn(() => false);
    render(<App />);
    await waitFor(() => expect(screen.getByText("Rust")).toBeTruthy());
    fireEvent.click(screen.getAllByRole("button", { name: "删除" })[0]);
    expect(api.deleteTag).not.toHaveBeenCalled();
  });
});

describe("文章编辑器标签选择", () => {
  const post = {
    id: "post-id",
    slug: "first",
    title: "标题",
    content: "正文",
    excerpt: null,
    status: "draft",
    visibility: "public" as const,
    version: 3,
    published_at: null,
    updated_at: "2026-09-22T00:00:00Z",
    author_id: "author-id",
    tag_ids: ["tag-rust"],
    category_id: null,
    series_id: null,
    series_order: null,
  };

  beforeEach(() => {
    window.history.replaceState(null, "", paths.editPost(post.slug));
    vi.mocked(api.listTags).mockResolvedValue([essay, rust]);
    vi.mocked(api.categoryApi.list).mockResolvedValue([]);
  vi.mocked(seriesApi.list).mockResolvedValue([]);
    const apiAny = api as unknown as Record<string, ReturnType<typeof vi.fn>>;
    apiAny.getPost = vi.fn().mockResolvedValue(post);
    apiAny.updatePost = vi.fn();
  });

  it("详情预选当前标签，勾选变化随保存提交", async () => {
    const apiAny = api as unknown as Record<string, ReturnType<typeof vi.fn>>;
    apiAny.updatePost.mockResolvedValue({ ...post, version: 4 });

    render(<App />);
    const rustBox = (await screen.findByLabelText("Rust")) as HTMLInputElement;
    const essayBox = screen.getByLabelText("随笔") as HTMLInputElement;
    expect(rustBox.checked).toBeTruthy();
    expect(essayBox.checked).toBeFalsy();

    fireEvent.click(essayBox);
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));

    await waitFor(() => {
      expect(apiAny.updatePost).toHaveBeenCalledWith(
        "first",
        expect.objectContaining({
          tag_ids: expect.arrayContaining(["tag-rust", "tag-essay"]),
          expected_version: 3,
        }),
      );
    });
  });
});
