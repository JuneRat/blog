// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError, api } from "../src/api";
import { navigate, paths } from "../src/router";
import type { PostDetail } from "../src/types";

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    status: "authenticated",
    me: { permissions: ["post.create", "post.publish"] },
  }),
}));

vi.mock("../src/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("../src/api")>();
  return {
    ...original,
    api: {
      getPost: vi.fn(), createPost: vi.fn(), updatePost: vi.fn(),
      publishPost: vi.fn(), unpublishPost: vi.fn(), listTags: vi.fn(),
    },
  };
});

const post: PostDetail = {
  id: "post-id", slug: "first", title: "原始标题", content: "原始正文",
  excerpt: null, status: "draft", visibility: "public", version: 1,
  published_at: null, updated_at: "2026-09-22T00:00:00Z", author_id: "author-id",
  tag_ids: [],
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function input(label: string): HTMLInputElement {
  return screen.getByLabelText(label) as HTMLInputElement;
}

beforeEach(() => {
  vi.resetAllMocks();
  window.history.replaceState(null, "", paths.editPost(post.slug));
  vi.mocked(api.getPost).mockResolvedValue(post);
  // 标签目录：空目录即可（编辑器只渲染选择区）。
  vi.mocked(api.listTags).mockResolvedValue([]);
});
afterEach(cleanup);

describe("文章编辑器回归", () => {
  it("创建跳转保留等待期间的输入，返回新建页则完整清空", async () => {
    window.history.replaceState(null, "", paths.newPost);
    const pending = deferred<PostDetail>();
    vi.mocked(api.createPost).mockReturnValue(pending.promise);
    render(<App />);
    fireEvent.change(input("标题"), { target: { value: post.title } });
    fireEvent.change(input("正文（Markdown）"), { target: { value: post.content } });
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));
    fireEvent.change(input("正文（Markdown）"), { target: { value: "等待期间的新输入" } });
    await act(async () => { pending.resolve(post); });
    expect(window.location.pathname).toBe(paths.editPost(post.slug));
    expect(input("正文（Markdown）").value).toBe("等待期间的新输入");
    expect(api.getPost).not.toHaveBeenCalled();

    // 模拟浏览器回到保存前的新建历史项；App 必须复用真实编辑组件。
    act(() => { navigate(paths.newPost); });
    expect(input("slug").value).toBe("");
    expect(input("标题").value).toBe("");
    expect(input("摘要").value).toBe("");
    expect(input("正文（Markdown）").value).toBe("");
    expect(screen.queryByText("v1")).toBeNull();
    expect(screen.queryByRole("button", { name: "发布" })).toBeNull();

    // 重新创建必须从空表单开始，不能沿用旧 slug 或正文。
    fireEvent.change(input("标题"), { target: { value: "第二篇" } });
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));
    expect(api.createPost).toHaveBeenLastCalledWith({
      slug: undefined, title: "第二篇", excerpt: undefined, content: "", visibility: "public",
      tag_ids: [],
    });
    await act(async () => {});
  });

  it("新建页不继承已发布文章的版本、可见性或冲突提示", async () => {
    vi.mocked(api.getPost).mockResolvedValue({ ...post, status: "published", visibility: "private", version: 7 });
    vi.mocked(api.updatePost).mockRejectedValue(new ApiError(409, "版本冲突", "version_conflict"));
    render(<App />);
    await screen.findByDisplayValue(post.title);
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));
    await screen.findByText("内容已在别处修改。");
    act(() => { navigate(paths.newPost); });
    expect(input("正文（Markdown）").value).toBe("");
    expect(input("可见性").value).toBe("public");
    expect(screen.getByText("草稿")).toBeTruthy();
    expect(screen.queryByText("v7")).toBeNull();
    expect(screen.queryByText("内容已在别处修改。")).toBeNull();
  });

  it("发布收到版本冲突后继续保留原正文和原版本", async () => {
    vi.mocked(api.publishPost).mockRejectedValue(new ApiError(409, "版本冲突", "version_conflict"));
    vi.mocked(api.updatePost).mockRejectedValue(new ApiError(409, "版本冲突", "version_conflict"));
    render(<App />);
    await screen.findByDisplayValue(post.title);
    fireEvent.click(screen.getByRole("button", { name: "发布" }));
    await screen.findByText("内容已在别处修改。");
    expect(input("正文（Markdown）").value).toBe(post.content);
    expect(screen.getByText("v1")).toBeTruthy();
    fireEvent.change(input("标题"), { target: { value: "新的标题" } });
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));
    await waitFor(() => expect(api.updatePost).toHaveBeenCalledWith(post.slug, expect.objectContaining({
      expected_version: 1, content: post.content,
    })));
  });

  it("改名后用新 slug 发布，路由更新仍保留等待期间的新编辑", async () => {
    const pending = deferred<PostDetail>();
    vi.mocked(api.updatePost).mockReturnValue(pending.promise);
    vi.mocked(api.publishPost).mockResolvedValue({ ...post, slug: "renamed", status: "published", version: 3 });
    render(<App />);
    await screen.findByDisplayValue(post.title);
    fireEvent.change(input("slug"), { target: { value: "renamed" } });
    fireEvent.click(screen.getByRole("button", { name: "发布" }));
    fireEvent.change(input("正文（Markdown）"), { target: { value: "继续编辑" } });
    await act(async () => { pending.resolve({ ...post, slug: "renamed", version: 2 }); });
    expect(api.publishPost).toHaveBeenCalledWith("renamed", 2);
    expect(window.location.pathname).toBe(paths.editPost("renamed"));
    expect(input("正文（Markdown）").value).toBe("继续编辑");
    expect(screen.getByText("已发布；等待期间的新改动尚未保存。")).toBeTruthy();
    expect(api.getPost).toHaveBeenCalledTimes(1);
  });
});
