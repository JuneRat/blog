// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError, api, categoryApi, seriesApi } from "../src/api";
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
    categoryApi: { list: vi.fn() },
    seriesApi: { list: vi.fn() },
    api: {
      getPost: vi.fn(), createPost: vi.fn(), updatePost: vi.fn(),
      publishPost: vi.fn(), unpublishPost: vi.fn(), listTags: vi.fn(),
      listPosts: vi.fn(),
    },
  };
});

const post: PostDetail = {
  id: "post-id", slug: "first", title: "原始标题", content: "原始正文",
  excerpt: null, status: "draft", visibility: "public", version: 1,
  published_at: null, updated_at: "2026-09-22T00:00:00Z", author_id: "author-id",
  tag_ids: [], category_id: null, series_id: null, series_order: null,
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
  // 编辑器读的是顶层 categoryApi（不是 api.categoryApi），mock 必须打在同一处。
  vi.mocked(categoryApi.list).mockResolvedValue([]);
  vi.mocked(seriesApi.list).mockResolvedValue([]);
  vi.mocked(api.listPosts).mockResolvedValue([]);
});
afterEach(cleanup);

describe("文章编辑器回归", () => {
  it("创建跳转保留等待期间的输入，返回新建页则完整清空", async () => {
    window.history.replaceState(null, "", paths.newPost);
    const pending = deferred<PostDetail>();
    vi.mocked(api.createPost).mockReturnValue(pending.promise);
    render(<App />);
    // 屏幕按路由懒加载：首屏渲染是 Suspense 占位，必须等真实编辑器挂载。
    await screen.findByLabelText("标题");
    fireEvent.change(input("标题"), { target: { value: post.title } });
    fireEvent.change(input("正文（Markdown）"), { target: { value: post.content } });
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));
    // antd Form 的 onFinish 在校验之后才触发；必须等请求真正发出，
    // 此时再输入才是「请求飞行期间」（见 PageEditScreen.test.tsx 的同名用例）。
    await waitFor(() => expect(api.createPost).toHaveBeenCalledTimes(1));
    fireEvent.change(input("正文（Markdown）"), { target: { value: "等待期间的新输入" } });
    await act(async () => { pending.resolve(post); });
    expect(window.location.pathname).toBe(paths.editPost(post.slug));
    expect(input("正文（Markdown）").value).toBe("等待期间的新输入");

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
    // onFinish 是异步的：先等第二次创建真正发出，再核对载荷。
    await waitFor(() => expect(api.createPost).toHaveBeenCalledTimes(2));
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
    // 可见性从原生 select 换成 antd Select：断言选中项文案（antd Select 不是原生控件）。
    expect(screen.getByTitle("公开")).toBeTruthy();
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
    // 发布按钮是同步 onClick（先保存再发布）：等 updatePost 真正发出后再改输入。
    await waitFor(() => expect(api.updatePost).toHaveBeenCalledTimes(1));
    fireEvent.change(input("正文（Markdown）"), { target: { value: "继续编辑" } });
    await act(async () => { pending.resolve({ ...post, slug: "renamed", version: 2 }); });
    expect(api.publishPost).toHaveBeenCalledWith("renamed", 2);
    expect(window.location.pathname).toBe(paths.editPost("renamed"));
    expect(input("正文（Markdown）").value).toBe("继续编辑");
    expect(screen.getByText("已发布；等待期间的新改动尚未保存。")).toBeTruthy();
  });

  // 回归：A 已加载 → B 加载失败时，表单里仍是 A 的正文与版本号。
  // 若不做 formMismatch 守卫，保存会把 A 的 expected_version 打到 B 的地址上，
  // 版本恰好相同就是静默覆盖 B。
  it("切换到加载失败的文章时拒绝保存与发布，重新加载后恢复", async () => {
    render(<App />);
    await screen.findByDisplayValue(post.title);

    vi.mocked(api.getPost).mockRejectedValueOnce(new ApiError(404, "未找到", "not_found"));
    act(() => { navigate(paths.editPost("missing")); });
    await screen.findByText(/文章未能加载/);
    // 表单仍是上一篇的内容（这正是危险所在）。
    expect(input("正文（Markdown）").value).toBe(post.content);
    expect(screen.getByText("v1")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));
    expect(await screen.findByText(/文章尚未成功加载/)).toBeTruthy();
    expect(api.updatePost).not.toHaveBeenCalled();
    expect(api.createPost).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "发布" }));
    expect(api.publishPost).not.toHaveBeenCalled();

    // 加载成功后恢复编辑与提交能力。
    vi.mocked(api.getPost).mockResolvedValueOnce({
      ...post, slug: "missing", title: "补回", version: 5,
    });
    fireEvent.click(screen.getByRole("button", { name: "重新加载" }));
    await screen.findByDisplayValue("补回");
    expect(screen.queryByText(/文章未能加载/)).toBeNull();
    expect(screen.getByText("v5")).toBeTruthy();
  });

  // 回归：列表查询有 30s 新鲜度且关闭了窗口聚焦重取。写后不失效缓存的话，
  // 「改完标题 → 返回列表」会命中旧缓存，看到保存前的标题与 slug。
  it("保存后返回列表看到新标题（写后必须失效列表缓存）", async () => {
    window.history.replaceState(null, "", paths.list);
    vi.mocked(api.listPosts).mockResolvedValue([{ ...post, title: "原始标题" }]);
    render(<App />);
    await screen.findByText("原始标题"); // 让列表缓存先落地

    act(() => { navigate(paths.editPost(post.slug)); });
    await screen.findByDisplayValue("原始标题");
    fireEvent.change(input("标题"), { target: { value: "改过的标题" } });
    vi.mocked(api.updatePost).mockResolvedValue({ ...post, title: "改过的标题", version: 2 });
    vi.mocked(api.listPosts).mockResolvedValue([{ ...post, title: "改过的标题", version: 2 }]);
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));
    await waitFor(() => expect(api.updatePost).toHaveBeenCalledTimes(1));

    // 走侧栏返回列表（真实路径；保存后没有未保存改动，不会弹确认）。
    fireEvent.click(screen.getByRole("menuitem", { name: "文章" }));
    expect(await screen.findByText("改过的标题")).toBeTruthy();
  });

  // 回归：有未保存改动时「发布」是**先保存、再改状态**。若只在状态切换成功后
  // 失效列表，保存成功而发布失败时，返回列表看到的还是保存前的数据。
  it("发布失败时，先行保存的结果仍会反映到列表", async () => {
    window.history.replaceState(null, "", paths.list);
    vi.mocked(api.listPosts).mockResolvedValue([{ ...post, title: "原始标题" }]);
    render(<App />);
    await screen.findByText("原始标题"); // 列表缓存先落地

    act(() => { navigate(paths.editPost(post.slug)); });
    await screen.findByDisplayValue("原始标题");
    fireEvent.change(input("标题"), { target: { value: "改过的标题" } });
    vi.mocked(api.updatePost).mockResolvedValue({ ...post, title: "改过的标题", version: 2 });
    vi.mocked(api.publishPost).mockRejectedValue(
      new ApiError(500, "发布失败", "internal", "req-9"),
    );
    vi.mocked(api.listPosts).mockResolvedValue([{ ...post, title: "改过的标题", version: 2 }]);

    fireEvent.click(screen.getByRole("button", { name: "发布" }));
    // 先行保存发出并成功（版本 2），随后发布失败。
    await waitFor(() => expect(api.updatePost).toHaveBeenCalledTimes(1));
    expect(api.publishPost).toHaveBeenCalledWith(post.slug, 2);
    expect(await screen.findByText(/发布失败（错误编号 req-9）/)).toBeTruthy();

    // 保存已经生效：返回列表必须是新标题。
    fireEvent.click(screen.getByRole("menuitem", { name: "文章" }));
    expect(await screen.findByText("改过的标题")).toBeTruthy();
  });
});
