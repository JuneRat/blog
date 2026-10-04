// @vitest-environment jsdom
import { contentPage } from "./contentFixtures";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ConfigProvider } from "antd";
import { App } from "../src/App";
import { ApiError } from "../src/api/client";
import { postsApi } from "../src/api/posts";
import { pagesApi } from "../src/api/pages";
import { tagsApi, categoryApi, seriesApi } from "../src/api/taxonomy";
import { navigate, paths } from "../src/router";
import type { PageDetail, PageSummary, PostDetail, PostSummary } from "../src/types";

/**
 * 三个列表屏的持久化行为测试：我的文章、独立页面、文章回收站。
 *
 * 这三个屏在「手写 CSS → antd v6」迁移时只做过临时冒烟验证，是仓库里唯一没有
 * 回归网的屏幕。测试统一渲染 `<App />`，因此拿到的是真实的 AdminProviders
 * （zh_CN locale、antd App 上下文、未保存改动登记），而不是裸屏幕组件。
 *
 * 约定（见 tests/tags.test.tsx、tests/unsaved.test.tsx）：
 * - 屏幕按路由懒加载，断言前必须 `await findXxx/waitFor`；
 * - 确认弹窗走 `App.useApp().modal.confirm`，按钮是「确定」/「取消」；antd 关闭后
 *   的 Modal 仍留在 DOM 里，所以同一条用例只开一次确认弹窗（确认/取消分用例）；
 * - 两字中文按钮不插空格，按无障碍名 "保存" 这类精确文本定位即可。
 */

/** 每个用例可改写的权限集合：屏幕按它决定操作入口是否渲染。 */
const state = vi.hoisted(() => ({
  permissions: ["post.create", "post.delete", "post.purge", "page.create"] as string[],
}));

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    status: "authenticated",
    me: { user_id: "me", permissions: state.permissions },
  }),
}));

vi.mock("../src/api/taxonomy", async (load) => {
  const original = await load<typeof import("../src/api/taxonomy")>();
  return { ...original, categoryApi: { list: vi.fn() }, seriesApi: { list: vi.fn() }, tagsApi: { ...original.tagsApi, listTags: vi.fn() } };
});
vi.mock("../src/api/posts", async (load) => {
  const original = await load<typeof import("../src/api/posts")>();
  return { ...original, postsApi: { ...original.postsApi, listPosts: vi.fn(), trashPost: vi.fn(), listTrash: vi.fn(), restorePost: vi.fn(), purgePost: vi.fn(), getPost: vi.fn(), batch: vi.fn() } };
});
vi.mock("../src/api/pages", async (load) => {
  const original = await load<typeof import("../src/api/pages")>();
  return { ...original, pagesApi: { ...original.pagesApi, listPages: vi.fn(), getPage: vi.fn(), updatePage: vi.fn(), publishPage: vi.fn(), unpublishPage: vi.fn() } };
});

function summary(overrides: Partial<PostSummary> = {}): PostSummary {
  return {
    id: "post-rust",
    slug: "rust-guide",
    title: "Rust 指南",
    status: "published",
    visibility: "public",
    version: 3,
    published_at: "2026-09-20T00:00:00Z",
    updated_at: "2026-09-22T10:00:00Z",
    author_id: "me",
    author_username: "author",
    ...overrides,
  };
}

/** 第一行：已发布、公开、v3。 */
const rustPost = summary();
/** 第二行：草稿、私有、v1、无标题（覆盖「（无标题）」占位）。 */
const draftPost = summary({
  id: "post-draft",
  slug: "draft-note",
  title: "",
  status: "draft",
  visibility: "private",
  version: 1,
});

const rustDetail: PostDetail = { tag_ids: [], category_id: null, series: [], cover_media_id: null, cover_url: null, ...rustPost, excerpt: null, content: "正文" };

const aboutPage: PageSummary = {
  id: "page-about",
  slug: "about",
  title: "关于",
  status: "published",
  visibility: "public",
  version: 2,
  published_at: "2026-09-20T00:00:00Z",
  updated_at: "2026-09-22T10:00:00Z",
};

const contactPage: PageSummary = {
  id: "page-contact",
  slug: "contact",
  title: "联系",
  status: "draft",
  visibility: "private",
  version: 1,
  published_at: null,
  updated_at: "2026-09-22T10:00:00Z",
};

const aboutDetail: PageDetail = { ...aboutPage, content: "正文" };

/** 回收站里唯一一条：slug gone、version 4。 */
const trashed = summary({ id: "post-gone", slug: "gone", title: "已删除的稿子", version: 4 });

beforeEach(() => {
  vi.resetAllMocks();
  state.permissions = ["post.create", "post.delete", "post.purge", "page.create"];
  vi.mocked(postsApi.listPosts).mockResolvedValue(contentPage([rustPost, draftPost]));
  vi.mocked(pagesApi.listPages).mockResolvedValue(contentPage([aboutPage, contactPage]));
  vi.mocked(postsApi.listTrash).mockResolvedValue({ items: [trashed], total: 1, page: 1, per_page: 10 });
  vi.mocked(postsApi.getPost).mockResolvedValue(rustDetail);
  vi.mocked(pagesApi.getPage).mockResolvedValue(aboutDetail);
  vi.mocked(tagsApi.listTags).mockResolvedValue([]);
  vi.mocked(categoryApi.list).mockResolvedValue([]);
  vi.mocked(seriesApi.list).mockResolvedValue([]);
  vi.mocked(postsApi.trashPost).mockResolvedValue(rustDetail);
  vi.mocked(postsApi.restorePost).mockResolvedValue(rustDetail);
  vi.mocked(postsApi.purgePost).mockResolvedValue(undefined);
  vi.mocked(postsApi.batch).mockResolvedValue({ items: [], affected: 0 });
});
afterEach(cleanup);

describe("我的文章列表", () => {
  beforeEach(() => {
    window.history.replaceState(null, "", paths.list);
  });

  it("按后端返回渲染标题、slug、状态、可见性与版本", async () => {
    render(<App />);

    expect(await screen.findByText("Rust 指南")).toBeTruthy();
    expect(screen.getByText("rust-guide")).toBeTruthy(); // slug 列
    expect(screen.getByText("draft-note")).toBeTruthy();
    expect(screen.getByText("（无标题）")).toBeTruthy(); // 空标题占位
    expect(screen.getByText("v3")).toBeTruthy(); // 版本列
    expect(screen.getByText("v1")).toBeTruthy();
    expect(screen.getByText("已发布")).toBeTruthy(); // status 映射
    expect(screen.getByText("草稿")).toBeTruthy();
    expect(screen.getByText("公开")).toBeTruthy(); // visibility 映射
    expect(screen.getByText("私有")).toBeTruthy();
  });

  it("搜索提交给服务器并重置页码，展示跨页匹配结果", async () => {
    window.history.replaceState(null, "", `${paths.list}?page=2`);
    vi.mocked(postsApi.listPosts).mockResolvedValue(contentPage([rustPost], 2, 25));
    render(<App />);
    await screen.findByText("Rust 指南");
    vi.mocked(postsApi.listPosts).mockResolvedValue(contentPage([draftPost], 1, 1));
    fireEvent.change(screen.getByRole("searchbox", { name: "搜索内容" }), { target: { value: "正文关键词" } });
    fireEvent.click(screen.getByRole("button", { name: "搜索" }));
    await waitFor(() => expect(postsApi.listPosts).toHaveBeenLastCalledWith({ page: 1, q: "正文关键词" }));
    await screen.findByText("draft-note");
    expect(screen.queryByText("Rust 指南")).toBeNull();
    expect(new URLSearchParams(window.location.search).get("q")).toBe("正文关键词");
  });

  it("有 post.create 权限时「新建草稿」导航到新建地址", async () => {
    render(<App />);
    await screen.findByText("Rust 指南");

    fireEvent.click(screen.getByRole("button", { name: "新建草稿" }));
    await waitFor(() => expect(window.location.pathname).toBe(paths.newPost));
  });

  it("整行点击进入该文章的编辑地址", async () => {
    render(<App />);

    fireEvent.click(await screen.findByText("Rust 指南"));
    await waitFor(() => expect(window.location.pathname).toBe(paths.editPost(rustPost.id)));
  });

  it("移入回收站：确认后以该行 slug 与当前版本调用 api，并刷新列表", async () => {
    render(<App />);
    await screen.findByText("Rust 指南");

    // 移入回收站后后端不再返回这一行：用它证明列表真的刷新了。
    vi.mocked(postsApi.listPosts).mockResolvedValue(contentPage([draftPost]));

    fireEvent.click(within(screen.getByRole("row", { name: /Rust 指南/ })).getByRole("button", { name: "移入回收站" }));
    // 确认弹窗标题带上文章标题，锁住「按行传参」而不是只按位置。
    expect(await screen.findByRole("dialog", { name: "将「Rust 指南」移入回收站？" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "确定" }));

    await waitFor(() => expect(postsApi.trashPost).toHaveBeenCalledWith(rustPost.id, 3));
    // 被移走的行从界面消失，另一行还在。
    await waitFor(() => expect(screen.queryByText("Rust 指南")).toBeNull());
    expect(screen.getByText("draft-note")).toBeTruthy();
    expect(window.location.pathname).toBe(paths.list); // 操作按钮不冒泡成一次跳转
  });

  it("移入回收站：取消时不发请求，也不跳转", async () => {
    render(<App />);
    await screen.findByText("Rust 指南");

    fireEvent.click(within(screen.getByRole("row", { name: /Rust 指南/ })).getByRole("button", { name: "移入回收站" }));
    fireEvent.click(await screen.findByRole("button", { name: "取消" }));

    expect(postsApi.trashPost).not.toHaveBeenCalled();
    expect(window.location.pathname).toBe(paths.list);
  });

  it("移入回收站失败时展示服务端文案，列表保持原样", async () => {
    vi.mocked(postsApi.trashPost).mockRejectedValue(
      new ApiError(409, "版本冲突：内容已被并发修改", "version_conflict", "req-2"),
    );
    render(<App />);
    await screen.findByText("Rust 指南");

    fireEvent.click(within(screen.getByRole("row", { name: /Rust 指南/ })).getByRole("button", { name: "移入回收站" }));
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));

    expect(await screen.findByText("版本冲突：内容已被并发修改（错误编号 req-2）")).toBeTruthy();
    // 失败不改动列表：原来两行都还在。
    expect(screen.getByText("Rust 指南")).toBeTruthy();
    expect(screen.getByText("draft-note")).toBeTruthy();
  });

  it("空列表展示带新建引导的空状态", async () => {
    vi.mocked(postsApi.listPosts).mockResolvedValue(contentPage([]));
    render(<App />);

    expect(await screen.findByText("还没有文章。点击「新建草稿」开始。")).toBeTruthy();
  });

  it("加载失败时展示错误文案与空状态", async () => {
    // 用不会重试的 4xx：QueryClient 只对 5xx 退避重试（1s/2s），
    // 换成 500 这条用例得等三次尝试才看得到错误态。
    vi.mocked(postsApi.listPosts).mockRejectedValue(
      new ApiError(403, "无权查看文章", "forbidden", "req-load"),
    );
    render(<App />);

    // ApiError 的 requestId 会拼进文案，锁住 withRequestId 契约。
    expect(await screen.findByText("没有权限：无权查看文章（错误编号 req-load）")).toBeTruthy();
    expect(screen.getByText("文章加载失败。")).toBeTruthy();
  });
});

describe("独立页面列表", () => {
  beforeEach(() => {
    window.history.replaceState(null, "", paths.pages);
  });

  it("按后端返回渲染标题、带斜杠的 slug、状态与版本", async () => {
    render(<App />);

    expect(await screen.findByText("关于")).toBeTruthy();
    expect(screen.getByText("/about")).toBeTruthy(); // slug 列带根路径前缀
    expect(screen.getByText("联系")).toBeTruthy();
    expect(screen.getByText("/contact")).toBeTruthy();
    expect(screen.getByText("v2")).toBeTruthy();
    expect(screen.getByText("v1")).toBeTruthy();
    expect(screen.getByText("已发布")).toBeTruthy();
    expect(screen.getByText("草稿")).toBeTruthy();
  });

  it("有 page.create 权限时「新建页面」导航到新建地址", async () => {
    render(<App />);
    await screen.findByText("关于");

    fireEvent.click(screen.getByRole("button", { name: "新建页面" }));
    await waitFor(() => expect(window.location.pathname).toBe(paths.newPage));
  });

  it("整行点击进入该页面的编辑地址", async () => {
    render(<App />);

    fireEvent.click(await screen.findByText("关于"));
    await waitFor(() => expect(window.location.pathname).toBe(paths.editPage(aboutPage.id)));
  });

  it("空列表展示带新建引导的空状态", async () => {
    vi.mocked(pagesApi.listPages).mockResolvedValue(contentPage([]));
    render(<App />);

    expect(await screen.findByText("还没有页面。点击「新建页面」开始。")).toBeTruthy();
  });

  it("加载失败时展示 403 前缀文案与空状态", async () => {
    vi.mocked(pagesApi.listPages).mockRejectedValue(
      new ApiError(403, "无权查看页面", "forbidden", null),
    );
    render(<App />);

    expect(await screen.findByText("没有权限：无权查看页面")).toBeTruthy();
    expect(screen.getByText("页面加载失败。")).toBeTruthy();
  });
});

describe("文章回收站", () => {
  beforeEach(() => {
    window.history.replaceState(null, "", paths.postTrash);
  });

  it("列出回收站条目并展示总数与页码", async () => {
    render(<App />);

    expect(await screen.findByText("已删除的稿子")).toBeTruthy();
    expect(screen.getByText("gone")).toBeTruthy();
    expect(screen.getByText("v4")).toBeTruthy();
    expect(screen.getByText("共 1 篇")).toBeTruthy();
    expect(screen.getByText("第 1 页")).toBeTruthy();
  });

  it("空回收站展示 0 篇且不能翻页", async () => {
    vi.mocked(postsApi.listTrash).mockResolvedValue({ items: [], total: 0, page: 1, per_page: 10 });
    render(<App />);

    expect(await screen.findByText("共 0 篇")).toBeTruthy();
    expect(screen.getByText("第 1 页")).toBeTruthy();
    expect(
      (screen.getByRole("button", { name: "下一页" }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it("恢复：以 ID 与当前版本调用 postsApi.restorePost，并刷新列表", async () => {
    render(<App />);
    await screen.findByText("已删除的稿子");

    // 恢复后后端不再返回这一行：用它证明列表真的刷新了。
    vi.mocked(postsApi.listTrash).mockResolvedValue({ items: [], total: 0, page: 1, per_page: 10 });

    fireEvent.click(screen.getByRole("button", { name: "恢复" }));

    await waitFor(() => expect(postsApi.restorePost).toHaveBeenCalledWith(trashed.id, 4));
    expect(await screen.findByText("共 0 篇")).toBeTruthy();
    expect(screen.queryByText("gone")).toBeNull();
    expect(screen.getByText("已恢复「已删除的稿子」。")).toBeTruthy();
  });

  it("永久删除：确认后以 ID 与当前版本调用 postsApi.purgePost", async () => {
    render(<App />);
    await screen.findByText("已删除的稿子");

    fireEvent.click(screen.getByRole("button", { name: "永久删除" }));
    expect(await screen.findByRole("dialog", { name: "永久删除「已删除的稿子」？" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "确定" }));

    await waitFor(() => expect(postsApi.purgePost).toHaveBeenCalledWith(trashed.id, 4));
  });

  it("永久删除：取消时不调用 postsApi.purgePost", async () => {
    render(<App />);
    await screen.findByText("已删除的稿子");

    fireEvent.click(screen.getByRole("button", { name: "永久删除" }));
    fireEvent.click(await screen.findByRole("button", { name: "取消" }));

    expect(postsApi.purgePost).not.toHaveBeenCalled();
  });

  it("分页：上一页/下一页切换到服务端回显的页码", async () => {
    // 服务端会回显本次返回的是第几页；界面页码以它为准，fixture 必须照实回显。
    vi.mocked(postsApi.listTrash).mockImplementation(async (target = 1) => ({
      items: [trashed],
      total: 25,
      page: target,
      per_page: 10,
    }));
    render(<App />);
    await screen.findByText("已删除的稿子");

    const prev = (): HTMLButtonElement =>
      screen.getByRole("button", { name: "上一页" }) as HTMLButtonElement;
    // 第 1 页不能再往前。
    expect(prev().disabled).toBe(true);

    fireEvent.click(screen.getByRole("button", { name: "下一页" }));
    expect(await screen.findByText("第 2 页")).toBeTruthy();

    // 请求结束后翻页才重新可用（请求中禁用翻页）。
    await waitFor(() => expect(prev().disabled).toBe(false));
    fireEvent.click(prev());
    expect(await screen.findByText("第 1 页")).toBeTruthy();
  });

  it("恢复失败时展示服务端文案与错误编号", async () => {
    vi.mocked(postsApi.restorePost).mockRejectedValue(
      new ApiError(409, "版本冲突：内容已被并发修改", "version_conflict", "req-3"),
    );
    render(<App />);
    await screen.findByText("已删除的稿子");

    fireEvent.click(screen.getByRole("button", { name: "恢复" }));

    expect(
      await screen.findByText("版本冲突：内容已被并发修改（错误编号 req-3）"),
    ).toBeTruthy();
  });

  it("加载失败时展示错误文案与空状态，且不渲染分页", async () => {
    vi.mocked(postsApi.listTrash).mockRejectedValue(
      new ApiError(409, "回收数据已被并发修改", "version_conflict", "req-7"),
    );
    render(<App />);

    expect(await screen.findByText(/回收数据已被并发修改（错误编号 req-7）/)).toBeTruthy();
    expect(screen.getByText("回收站加载失败。")).toBeTruthy();
    expect(screen.queryByText(/第 1 页/)).toBeNull();
  });

  it("操作成功但重载失败时保留列表，并同时给出成功与失败提示", async () => {
    render(<App />);
    await screen.findByText("已删除的稿子");

    // 第一次列表拉取正常，操作后的那次重载失败。
    // 用不会重试的 4xx：QueryClient 只对 5xx 退避重试，5xx 会把失败推迟几秒。
    vi.mocked(postsApi.listTrash).mockRejectedValueOnce(
      new ApiError(409, "回收数据已被并发修改", "version_conflict", "req-9"),
    );
    fireEvent.click(screen.getByRole("button", { name: "恢复" }));

    expect(await screen.findByText("已恢复「已删除的稿子」。")).toBeTruthy();
    expect(await screen.findByText(/回收数据已被并发修改（错误编号 req-9）/)).toBeTruthy();
    // 行还在：重载失败不等于操作失败，不能把已成功的行抹掉。
    expect(screen.getByText("已删除的稿子")).toBeTruthy();
  });

  it("删除当前页最后一条后回退一页，而不是停在空页", async () => {
    const second = summary({ id: "post-2", slug: "second", title: "第二页的稿子", version: 2 });
    vi.mocked(postsApi.listTrash).mockImplementation(async (target = 1) =>
      target === 1
        ? { items: [trashed], total: 11, page: 1, per_page: 10 }
        : { items: [second], total: 11, page: 2, per_page: 10 },
    );
    render(<App />);
    await screen.findByText("已删除的稿子");

    fireEvent.click(screen.getByRole("button", { name: "下一页" }));
    expect(await screen.findByText("第二页的稿子")).toBeTruthy();
    expect(screen.getByText("第 2 页")).toBeTruthy();

    // 第 2 页唯一一条被删除后，服务端该页为空 → 必须回退到第 1 页。
    vi.mocked(postsApi.listTrash).mockImplementation(async (target = 1) =>
      target === 1
        ? { items: [trashed], total: 10, page: 1, per_page: 10 }
        : { items: [], total: 10, page: 2, per_page: 10 },
    );
    fireEvent.click(within(screen.getByRole("row", { name: /第二页的稿子/ })).getByRole("button", { name: "永久删除" }));
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));

    // 先等目标页的行出现：页码由服务端回显渲染，等「第 1 页」等于等数据到位。
    expect(await screen.findByText("已删除的稿子")).toBeTruthy();
    expect(screen.getByText("第 1 页")).toBeTruthy();
    expect(screen.queryByText("第二页的稿子")).toBeNull();
  });

  it("403 时给出「没有权限：」前缀（与文章/页面列表同口径）", async () => {
    vi.mocked(postsApi.restorePost).mockRejectedValue(
      new ApiError(403, "无权执行该操作", "forbidden", "req-11"),
    );
    render(<App />);
    await screen.findByText("已删除的稿子");

    fireEvent.click(screen.getByRole("button", { name: "恢复" }));

    expect(await screen.findByText("没有权限：无权执行该操作（错误编号 req-11）")).toBeTruthy();
  });

});

describe("跨屏缓存一致性", () => {
  // 回归：恢复会让文章回到「我的文章」列表，所以恢复后也必须失效文章列表缓存，
  // 否则（列表缓存 30s 内仍然新鲜）回去看不到刚恢复的那篇。
  it("回收站恢复后，文章列表能看到它", async () => {
    window.history.replaceState(null, "", paths.list);
    vi.mocked(postsApi.listPosts).mockResolvedValue(contentPage([draftPost]));
    render(<App />);
    await screen.findByText("draft-note"); // 列表缓存先落地（此时没有那篇）
    expect(screen.queryByText("已删除的稿子")).toBeNull();

    act(() => { navigate(paths.postTrash); });
    await screen.findByText("已删除的稿子");
    vi.mocked(postsApi.listPosts).mockResolvedValue(contentPage([draftPost, trashed]));
    fireEvent.click(screen.getByRole("button", { name: "恢复" }));
    await screen.findByText("已恢复「已删除的稿子」。");

    act(() => { navigate(paths.list); });
    expect(await screen.findByText("已删除的稿子")).toBeTruthy();
  });

  // 与文章编辑器对称：页面保存后返回列表也不能看到旧标题。
  it("页面保存后，页面列表看到新标题", async () => {
    window.history.replaceState(null, "", paths.pages);
    vi.mocked(pagesApi.listPages).mockResolvedValue(contentPage([aboutPage]));
    render(<App />);
    await screen.findByText("关于"); // 列表缓存先落地

    act(() => { navigate(paths.editPage(aboutPage.id)); });
    await screen.findByDisplayValue("关于");
    fireEvent.change(screen.getByLabelText("标题"), { target: { value: "关于我们" } });
    vi.mocked(pagesApi.updatePage).mockResolvedValue({ ...aboutDetail, title: "关于我们", version: 3 });
    vi.mocked(pagesApi.listPages).mockResolvedValue(contentPage([{ ...aboutPage, title: "关于我们", version: 3 }]));
    fireEvent.click(screen.getByRole("button", { name: /保存草稿|保存预约内容|保存修改草稿/ }));
    await waitFor(() => expect(pagesApi.updatePage).toHaveBeenCalled());

    fireEvent.click(screen.getByRole("menuitem", { name: "独立页面" }));
    expect(await screen.findByText("关于我们")).toBeTruthy();
  });

  // 撤回不应将编辑器里的未公开修改先更新到线上。
  it("页面撤回失败时，保留本地输入且列表保持服务器内容", async () => {
    // aboutPage 是已发布状态，按钮是「撤回为草稿」，需要 page.unpublish（只影响本条用例）。
    state.permissions = ["page.create", "page.unpublish"];
    window.history.replaceState(null, "", paths.pages);
    vi.mocked(pagesApi.listPages).mockResolvedValue(contentPage([aboutPage]));
    render(<App />);
    await screen.findByText("关于"); // 列表缓存先落地

    act(() => { navigate(paths.editPage(aboutPage.id)); });
    await screen.findByDisplayValue("关于");
    fireEvent.change(screen.getByLabelText("标题"), { target: { value: "关于我们" } });
    vi.mocked(pagesApi.updatePage).mockResolvedValue({ ...aboutDetail, title: "关于我们", version: 3 });
    vi.mocked(pagesApi.unpublishPage).mockRejectedValue(
      new ApiError(500, "撤回失败", "internal", "req-8"),
    );

    fireEvent.click(screen.getByRole("button", { name: "撤回为草稿" }));
    await waitFor(() => expect(pagesApi.unpublishPage).toHaveBeenCalledWith(aboutPage.id, aboutDetail.version));
    expect(pagesApi.updatePage).not.toHaveBeenCalled();
    expect(await screen.findByText(/撤回失败（错误编号 req-8）/)).toBeTruthy();

    fireEvent.click(screen.getByRole("menuitem", { name: "独立页面" }));
    fireEvent.click(await screen.findByRole("button", { name: "放弃修改并离开" }));
    expect(await screen.findByText("关于")).toBeTruthy();
    expect(screen.queryByText("关于我们")).toBeNull();
  });
});

describe("内容服务端分页", () => {
  it.each(["post", "page"] as const)("%s 翻页期间保持页码与行一致，切换筛选回到首页", async kind => {
    const isPost = kind === "post";
    const listing = isPost ? vi.mocked(postsApi.listPosts) : vi.mocked(pagesApi.listPages);
    const first = isPost ? rustPost : aboutPage;
    const second = isPost ? draftPost : contactPage;
    // 两种摘要的共同展示字段一致；文章摘要保留作者字段。
    listing.mockResolvedValue(contentPage([first as PostSummary], 1, 21));
    window.history.replaceState(null, "", isPost ? paths.list : paths.pages);
    render(<App />);
    await screen.findByText(first.title);
    let resolve!: (value: ReturnType<typeof contentPage<PostSummary>>) => void;
    listing.mockImplementationOnce(() => new Promise(done => { resolve = done; }));
    fireEvent.click(screen.getByRole("button", { name: "下一页" }));
    await waitFor(() => expect(listing).toHaveBeenLastCalledWith({ page: 2 }));
    expect(screen.getByText(first.title)).toBeTruthy();
    await act(async () => resolve(contentPage([second as PostSummary], 2, 21)));
    await screen.findByText(isPost ? second.slug : second.title);
    listing.mockResolvedValue(contentPage([], 1, 0));
    fireEvent.mouseDown(screen.getByRole("combobox", { name: "筛选状态" }));
    fireEvent.click(await screen.findByText("已归档"));
    await waitFor(() => expect(listing).toHaveBeenLastCalledWith({ page: 1, status: "archived" }));
    await waitFor(() => expect(screen.queryByText(isPost ? second.slug : second.title)).toBeNull());
  });

  it("删除第二页最后一篇后回退首页，并重新读取已缓存的首页", async () => {
    window.history.replaceState(null, "", paths.list);
    vi.mocked(postsApi.listPosts).mockImplementation(async filter => contentPage([filter?.page === 2 ? draftPost : rustPost], filter?.page ?? 1, 21));
    render(<App />);
    await screen.findByText("Rust 指南");
    fireEvent.click(screen.getByRole("button", { name: "下一页" }));
    await screen.findByText(draftPost.slug);
    fireEvent.click(screen.getByRole("button", { name: "移入回收站" }));
    await screen.findByRole("dialog");
    vi.mocked(postsApi.listPosts).mockImplementation(async filter => contentPage(filter?.page === 2 ? [] : [{ ...rustPost, title: "刷新后的首页" }], filter?.page ?? 1, 20));
    fireEvent.click(screen.getByRole("button", { name: "确定" }));
    await screen.findByText("刷新后的首页");
    expect(new URLSearchParams(window.location.search).get("page")).toBe("1");
    expect(postsApi.trashPost).toHaveBeenCalledWith(draftPost.id, draftPost.version);
  });
});


describe("跨页搜索与协作范围", () => {
  it("编辑切换全部文章并按作者筛选", async () => {
    window.history.replaceState(null, "", paths.list);
    state.permissions = ["post.read_any", "post.update_any", "post.delete_any"];
    render(<App />);
    await screen.findByText("Rust 指南");
    fireEvent.mouseDown(screen.getByRole("combobox", { name: "文章范围" }));
    fireEvent.click(await screen.findByText("全部文章"));
    await waitFor(() => expect(postsApi.listPosts).toHaveBeenLastCalledWith({ page: 1, scope: "all" }));
    fireEvent.change(screen.getByRole("searchbox", { name: "筛选作者" }), { target: { value: "disabled-author" } });
    fireEvent.click(screen.getByRole("button", { name: "筛选作者" }));
    await waitFor(() => expect(postsApi.listPosts).toHaveBeenLastCalledWith({ page: 1, scope: "all", author: "disabled-author" }));
  });

  it("从编辑器返回列表保留搜索和页码，刷新所需参数也留在地址中", async () => {
    window.history.replaceState(null, "", `${paths.list}?page=2&q=Rust`);
    vi.mocked(postsApi.listPosts).mockResolvedValue(contentPage([rustPost], 2, 25));
    render(<App />);
    await screen.findByText("Rust 指南");
    fireEvent.click(screen.getByRole("button", { name: "编辑" }));
    await screen.findByLabelText("正文（Markdown）");
    await act(async () => navigate(paths.list));
    await waitFor(() => expect(postsApi.listPosts).toHaveBeenLastCalledWith(expect.objectContaining({ page: 2, q: "Rust" })));
    expect((screen.getByRole("searchbox", { name: "搜索内容" }) as HTMLInputElement).value).toBe("Rust");
    await waitFor(() => expect(new URLSearchParams(window.location.search).get("page")).toBe("2"));
    expect(new URLSearchParams(window.location.search).get("q")).toBe("Rust");
  });

  it("页面列表同样提交服务端搜索", async () => {
    window.history.replaceState(null, "", paths.pages);
    render(<App />);
    await screen.findByText("关于");
    fireEvent.change(screen.getByRole("searchbox", { name: "搜索内容" }), { target: { value: "联系" } });
    fireEvent.click(screen.getByRole("button", { name: "搜索" }));
    await waitFor(() => expect(pagesApi.listPages).toHaveBeenLastCalledWith({ page: 1, q: "联系" }));
  });
});

describe("文章列表批量操作", () => {
  beforeEach(() => {
    window.history.replaceState(null, "", paths.list);
    state.permissions = ["post.create", "post.delete", "post.publish", "post.unpublish", "post.update"];
  });

  it("批量移入回收站：调用 postsApi.batch 并刷新列表", async () => {
    render(<App />);
    await screen.findByText("Rust 指南");
    const checkboxes = screen.getAllByRole("checkbox");
    fireEvent.click(checkboxes[1]);
    expect(await screen.findByText(/已选 1 篇：/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "批量移入回收站" }));
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));

    await waitFor(() => expect(postsApi.batch).toHaveBeenCalledWith({
      action: "trash",
      items: [{ id: rustPost.id, expected_version: rustPost.version }],
    }));
  });

  it("批量发布：调用 postsApi.batch status=published", async () => {
    render(<App />);
    await screen.findByText("Rust 指南");
    const checkboxes = screen.getAllByRole("checkbox");
    fireEvent.click(checkboxes[1]);
    fireEvent.click(screen.getByRole("button", { name: "批量发布" }));

    await waitFor(() => expect(postsApi.batch).toHaveBeenCalledWith({
      action: "change_status",
      params: { status: "published" },
      items: [{ id: rustPost.id, expected_version: rustPost.version }],
    }));
  });

  it("批量撤回草稿：调用 postsApi.batch status=draft", async () => {
    render(<App />);
    await screen.findByText("Rust 指南");
    const checkboxes = screen.getAllByRole("checkbox");
    fireEvent.click(checkboxes[1]);
    fireEvent.click(screen.getByRole("button", { name: "批量撤回草稿" }));

    await waitFor(() => expect(postsApi.batch).toHaveBeenCalledWith({
      action: "change_status",
      params: { status: "draft" },
      items: [{ id: rustPost.id, expected_version: rustPost.version }],
    }));
  });

  it("批量修改分类：未选择时禁用保存，明确选择清除时提交 null", async () => {
    vi.mocked(categoryApi.list).mockResolvedValue([
      { id: "cat-1", name: "后端技术", slug: "backend", parent_id: null, description: null, version: 1, pub_post_count: 5 },
    ]);
    render(<App />);
    await screen.findByText("Rust 指南");
    const checkboxes = screen.getAllByRole("checkbox");
    fireEvent.click(checkboxes[1]);
    fireEvent.click(screen.getByRole("button", { name: "批量修改分类" }));

    const saveBtn = await screen.findByRole("button", { name: "保存" });
    expect(saveBtn.hasAttribute("disabled")).toBe(true);

    const select = screen.getByRole("combobox", { name: "" });
    fireEvent.mouseDown(select);
    fireEvent.click(await screen.findByText("（清除分类）"));

    expect(saveBtn.hasAttribute("disabled")).toBe(false);
    fireEvent.click(saveBtn);

    await waitFor(() => expect(postsApi.batch).toHaveBeenCalledWith({
      action: "change_category",
      params: { category_id: null },
      items: [{ id: rustPost.id, expected_version: rustPost.version }],
    }));
  });

  it("分类更新冲突后关闭弹窗，显示错误并在重新选择后提交最新版本", async () => {
    vi.mocked(categoryApi.list).mockResolvedValue([
      { id: "cat-1", name: "后端技术", slug: "backend", parent_id: null, description: null, version: 1, pub_post_count: 5 },
    ]);
    vi.mocked(postsApi.listPosts)
      .mockResolvedValueOnce(contentPage([rustPost]))
      .mockResolvedValue(contentPage([{ ...rustPost, version: 4 }]));
    vi.mocked(postsApi.batch)
      .mockRejectedValueOnce(new ApiError(409, "分类版本冲突", "version_conflict", "req-category-conflict"))
      .mockResolvedValue({ items: [{ id: rustPost.id, version: 5, changed: true }], affected: 1 });

    // jsdom does not complete CSS animations; assert the actual closed state.
    render(<ConfigProvider theme={{ token: { motion: false } }}><App /></ConfigProvider>);
    await screen.findByText("v3");
    fireEvent.click(screen.getAllByRole("checkbox")[1]);
    fireEvent.click(screen.getByRole("button", { name: "批量修改分类" }));
    let dialog = await screen.findByRole("dialog");
    fireEvent.mouseDown(within(dialog).getByRole("combobox"));
    fireEvent.click(await screen.findByText("（清除分类）"));
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(await screen.findByText("分类版本冲突（错误编号 req-category-conflict）")).toBeTruthy();
    await screen.findByText("v4");
    expect(screen.queryByRole("button", { name: "批量修改分类" })).toBeNull();
    expect(postsApi.batch).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getAllByRole("checkbox")[1]);
    const editCategory = screen.getByRole("button", { name: "批量修改分类" });
    await waitFor(() => expect(editCategory.hasAttribute("disabled")).toBe(false));
    fireEvent.click(editCategory);
    dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByRole("button", { name: "保存" }).hasAttribute("disabled")).toBe(true);
    fireEvent.mouseDown(within(dialog).getByRole("combobox"));
    fireEvent.click(await screen.findByText("后端技术"));
    fireEvent.click(within(dialog).getByRole("button", { name: "保存" }));

    await waitFor(() => expect(postsApi.batch).toHaveBeenLastCalledWith({
      action: "change_category",
      params: { category_id: "cat-1" },
      items: [{ id: rustPost.id, expected_version: 4 }],
    }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("409 冲突后刷新文章列表并清空选中项", async () => {
    vi.mocked(postsApi.batch).mockRejectedValue(
      new ApiError(409, "版本冲突：内容已被并发修改", "version_conflict", "req-batch-conflict"),
    );
    render(<App />);
    await screen.findByText("Rust 指南");
    const checkboxes = screen.getAllByRole("checkbox");
    fireEvent.click(checkboxes[1]);
    expect(await screen.findByText(/已选 1 篇：/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "批量发布" }));

    expect(await screen.findByText("版本冲突：内容已被并发修改（错误编号 req-batch-conflict）")).toBeTruthy();
    await waitFor(() => expect(screen.queryByText(/已选 1 篇：/)).toBeNull());
    expect(postsApi.listPosts).toHaveBeenCalledTimes(2);
  });
});

describe("回收站批量操作", () => {
  beforeEach(() => {
    window.history.replaceState(null, "", paths.postTrash);
    state.permissions = ["post.delete", "post.purge"];
  });

  it("批量恢复：调用 postsApi.batch action=restore", async () => {
    render(<App />);
    await screen.findByText("已删除的稿子");
    const checkboxes = screen.getAllByRole("checkbox");
    fireEvent.click(checkboxes[1]);
    expect(await screen.findByText(/已选 1 篇：/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "批量恢复" }));
    await waitFor(() => expect(postsApi.batch).toHaveBeenCalledWith({
      action: "restore",
      items: [{ id: trashed.id, expected_version: trashed.version }],
    }));
  });

  it("批量永久删除：确认后调用 postsApi.batch action=purge", async () => {
    render(<App />);
    await screen.findByText("已删除的稿子");
    const checkboxes = screen.getAllByRole("checkbox");
    fireEvent.click(checkboxes[1]);
    expect(await screen.findByText(/已选 1 篇：/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "批量永久删除" }));
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));
    await waitFor(() => expect(postsApi.batch).toHaveBeenCalledWith({
      action: "purge",
      items: [{ id: trashed.id, expected_version: trashed.version }],
    }));
  });

  it("409 冲突后刷新回收站列表并清空选中项", async () => {
    vi.mocked(postsApi.batch).mockRejectedValue(
      new ApiError(409, "版本冲突：内容已被并发修改", "version_conflict", "req-trash-conflict"),
    );
    render(<App />);
    await screen.findByText("已删除的稿子");
    const checkboxes = screen.getAllByRole("checkbox");
    fireEvent.click(checkboxes[1]);
    expect(await screen.findByText(/已选 1 篇：/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "批量恢复" }));

    expect(await screen.findByText("版本冲突：内容已被并发修改（错误编号 req-trash-conflict）")).toBeTruthy();
    await waitFor(() => expect(screen.queryByText(/已选 1 篇：/)).toBeNull());
    expect(postsApi.listTrash).toHaveBeenCalledTimes(2);
  });
});
