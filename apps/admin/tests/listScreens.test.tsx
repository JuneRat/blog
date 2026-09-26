// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError, api, categoryApi, seriesApi } from "../src/api";
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

vi.mock("../src/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("../src/api")>();
  return {
    ...original,
    categoryApi: { list: vi.fn() },
    seriesApi: { list: vi.fn() },
    api: {
      ...original.api,
      listPosts: vi.fn(),
      trashPost: vi.fn(),
      listPages: vi.fn(),
      getPage: vi.fn(),
      updatePage: vi.fn(),
      publishPage: vi.fn(),
      unpublishPage: vi.fn(),
      listTrash: vi.fn(),
      restorePost: vi.fn(),
      purgePost: vi.fn(),
      getPost: vi.fn(),
      listTags: vi.fn(),
    },
  };
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
    tag_ids: [],
    category_id: null,
    series_id: null,
    series_order: null,
    cover_media_id: null,
    cover_url: null,
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

const rustDetail: PostDetail = { ...rustPost, excerpt: null, content: "正文" };

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
  vi.mocked(api.listPosts).mockResolvedValue([rustPost, draftPost]);
  vi.mocked(api.listPages).mockResolvedValue([aboutPage, contactPage]);
  vi.mocked(api.listTrash).mockResolvedValue({ items: [trashed], total: 1, page: 1, per_page: 10 });
  vi.mocked(api.getPost).mockResolvedValue(rustDetail);
  vi.mocked(api.getPage).mockResolvedValue(aboutDetail);
  vi.mocked(api.listTags).mockResolvedValue([]);
  vi.mocked(categoryApi.list).mockResolvedValue([]);
  vi.mocked(seriesApi.list).mockResolvedValue([]);
  vi.mocked(api.trashPost).mockResolvedValue(rustDetail);
  vi.mocked(api.restorePost).mockResolvedValue(rustDetail);
  vi.mocked(api.purgePost).mockResolvedValue(undefined);
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
    vi.mocked(api.listPosts).mockResolvedValue([draftPost]);

    // 后端返回顺序即渲染顺序，第一行是 rust-guide。
    fireEvent.click(screen.getAllByRole("button", { name: "移入回收站" })[0]);
    // 确认弹窗标题带上文章标题，锁住「按行传参」而不是只按位置。
    expect(await screen.findByRole("dialog", { name: "将「Rust 指南」移入回收站？" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "确定" }));

    await waitFor(() => expect(api.trashPost).toHaveBeenCalledWith(rustPost.id, 3));
    // 被移走的行从界面消失，另一行还在。
    await waitFor(() => expect(screen.queryByText("Rust 指南")).toBeNull());
    expect(screen.getByText("draft-note")).toBeTruthy();
    expect(window.location.pathname).toBe(paths.list); // 操作按钮不冒泡成一次跳转
  });

  it("移入回收站：取消时不发请求，也不跳转", async () => {
    render(<App />);
    await screen.findByText("Rust 指南");

    fireEvent.click(screen.getAllByRole("button", { name: "移入回收站" })[0]);
    fireEvent.click(await screen.findByRole("button", { name: "取消" }));

    expect(api.trashPost).not.toHaveBeenCalled();
    expect(window.location.pathname).toBe(paths.list);
  });

  it("无 post.delete / post.create 权限时不渲染操作入口，也不调用接口", async () => {
    state.permissions = [];
    render(<App />);
    await screen.findByText("Rust 指南");

    expect(screen.queryByRole("button", { name: "移入回收站" })).toBeNull();
    expect(screen.queryByRole("button", { name: "新建草稿" })).toBeNull();
    expect(api.trashPost).not.toHaveBeenCalled();
  });

  it("移入回收站失败时展示服务端文案，列表保持原样", async () => {
    vi.mocked(api.trashPost).mockRejectedValue(
      new ApiError(409, "版本冲突：内容已被并发修改", "version_conflict", "req-2"),
    );
    render(<App />);
    await screen.findByText("Rust 指南");

    fireEvent.click(screen.getAllByRole("button", { name: "移入回收站" })[0]);
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));

    expect(await screen.findByText("版本冲突：内容已被并发修改（错误编号 req-2）")).toBeTruthy();
    // 失败不改动列表：原来两行都还在。
    expect(screen.getByText("Rust 指南")).toBeTruthy();
    expect(screen.getByText("draft-note")).toBeTruthy();
  });

  it("空列表展示带新建引导的空状态", async () => {
    vi.mocked(api.listPosts).mockResolvedValue([]);
    render(<App />);

    expect(await screen.findByText("还没有文章。点击「新建草稿」开始。")).toBeTruthy();
  });

  it("加载失败时展示错误文案与空状态", async () => {
    // 用不会重试的 4xx：QueryClient 只对 5xx 退避重试（1s/2s），
    // 换成 500 这条用例得等三次尝试才看得到错误态。
    vi.mocked(api.listPosts).mockRejectedValue(
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

  it("无 page.create 权限时不渲染「新建页面」入口", async () => {
    state.permissions = ["post.create"];
    render(<App />);
    await screen.findByText("关于");

    expect(screen.queryByRole("button", { name: "新建页面" })).toBeNull();
  });

  it("空列表展示带新建引导的空状态", async () => {
    vi.mocked(api.listPages).mockResolvedValue([]);
    render(<App />);

    expect(await screen.findByText("还没有页面。点击「新建页面」开始。")).toBeTruthy();
  });

  it("加载失败时展示 403 前缀文案与空状态", async () => {
    vi.mocked(api.listPages).mockRejectedValue(
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
    vi.mocked(api.listTrash).mockResolvedValue({ items: [], total: 0, page: 1, per_page: 10 });
    render(<App />);

    expect(await screen.findByText("共 0 篇")).toBeTruthy();
    expect(screen.getByText("第 1 页")).toBeTruthy();
    expect(
      (screen.getByRole("button", { name: "下一页" }) as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it("恢复：以 ID 与当前版本调用 api.restorePost，并刷新列表", async () => {
    render(<App />);
    await screen.findByText("已删除的稿子");

    // 恢复后后端不再返回这一行：用它证明列表真的刷新了。
    vi.mocked(api.listTrash).mockResolvedValue({ items: [], total: 0, page: 1, per_page: 10 });

    fireEvent.click(screen.getByRole("button", { name: "恢复" }));

    await waitFor(() => expect(api.restorePost).toHaveBeenCalledWith(trashed.id, 4));
    expect(await screen.findByText("共 0 篇")).toBeTruthy();
    expect(screen.queryByText("gone")).toBeNull();
  });

  it("永久删除：确认后以 ID 与当前版本调用 api.purgePost", async () => {
    render(<App />);
    await screen.findByText("已删除的稿子");

    fireEvent.click(screen.getByRole("button", { name: "永久删除" }));
    expect(await screen.findByRole("dialog", { name: "永久删除「已删除的稿子」？" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "确定" }));

    await waitFor(() => expect(api.purgePost).toHaveBeenCalledWith(trashed.id, 4));
  });

  it("永久删除：取消时不调用 api.purgePost", async () => {
    render(<App />);
    await screen.findByText("已删除的稿子");

    fireEvent.click(screen.getByRole("button", { name: "永久删除" }));
    fireEvent.click(await screen.findByRole("button", { name: "取消" }));

    expect(api.purgePost).not.toHaveBeenCalled();
  });

  it("无 post.purge 权限时不渲染「永久删除」，但恢复仍可用", async () => {
    state.permissions = ["post.create"];
    render(<App />);
    await screen.findByText("已删除的稿子");

    expect(screen.queryByRole("button", { name: "永久删除" })).toBeNull();
    expect(screen.getByRole("button", { name: "恢复" })).toBeTruthy();
  });

  it("分页：上一页/下一页切换到服务端回显的页码", async () => {
    // 服务端会回显本次返回的是第几页；界面页码以它为准，fixture 必须照实回显。
    vi.mocked(api.listTrash).mockImplementation(async (target: number) => ({
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
    vi.mocked(api.restorePost).mockRejectedValue(
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
    vi.mocked(api.listTrash).mockRejectedValue(
      new ApiError(409, "回收数据已被并发修改", "version_conflict", "req-7"),
    );
    render(<App />);

    expect(await screen.findByText(/回收数据已被并发修改（错误编号 req-7）/)).toBeTruthy();
    expect(screen.getByText("回收站加载失败。")).toBeTruthy();
    expect(screen.queryByText(/第 1 页/)).toBeNull();
  });

  // 以下三条覆盖「操作成功 → 重载」这段的边界：重载失败不能看起来像操作失败，
  // 也不能让成功的行凭空消失；当前页被清空时要回退而不是停在空页。
  it("恢复成功后给出成功提示", async () => {
    render(<App />);
    await screen.findByText("已删除的稿子");

    fireEvent.click(screen.getByRole("button", { name: "恢复" }));

    expect(await screen.findByText("已恢复「已删除的稿子」。")).toBeTruthy();
  });

  it("操作成功但重载失败时保留列表，并同时给出成功与失败提示", async () => {
    render(<App />);
    await screen.findByText("已删除的稿子");

    // 第一次列表拉取正常，操作后的那次重载失败。
    // 用不会重试的 4xx：QueryClient 只对 5xx 退避重试，5xx 会把失败推迟几秒。
    vi.mocked(api.listTrash).mockRejectedValueOnce(
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
    vi.mocked(api.listTrash).mockImplementation(async (target: number) =>
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
    vi.mocked(api.listTrash).mockImplementation(async (target: number) =>
      target === 1
        ? { items: [trashed], total: 10, page: 1, per_page: 10 }
        : { items: [], total: 10, page: 2, per_page: 10 },
    );
    fireEvent.click(screen.getAllByRole("button", { name: "永久删除" })[0]);
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));

    // 先等目标页的行出现：页码由服务端回显渲染，等「第 1 页」等于等数据到位。
    expect(await screen.findByText("已删除的稿子")).toBeTruthy();
    expect(screen.getByText("第 1 页")).toBeTruthy();
    expect(screen.queryByText("第二页的稿子")).toBeNull();
  });

  it("403 时给出「没有权限：」前缀（与文章/页面列表同口径）", async () => {
    vi.mocked(api.restorePost).mockRejectedValue(
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
    vi.mocked(api.listPosts).mockResolvedValue([draftPost]);
    render(<App />);
    await screen.findByText("draft-note"); // 列表缓存先落地（此时没有那篇）
    expect(screen.queryByText("已删除的稿子")).toBeNull();

    act(() => { navigate(paths.postTrash); });
    await screen.findByText("已删除的稿子");
    vi.mocked(api.listPosts).mockResolvedValue([draftPost, trashed]);
    fireEvent.click(screen.getByRole("button", { name: "恢复" }));
    await screen.findByText("已恢复「已删除的稿子」。");

    act(() => { navigate(paths.list); });
    expect(await screen.findByText("已删除的稿子")).toBeTruthy();
  });

  // 与文章编辑器对称：页面保存后返回列表也不能看到旧标题。
  it("页面保存后，页面列表看到新标题", async () => {
    window.history.replaceState(null, "", paths.pages);
    vi.mocked(api.listPages).mockResolvedValue([aboutPage]);
    render(<App />);
    await screen.findByText("关于"); // 列表缓存先落地

    act(() => { navigate(paths.editPage(aboutPage.id)); });
    await screen.findByDisplayValue("关于");
    fireEvent.change(screen.getByLabelText("标题"), { target: { value: "关于我们" } });
    vi.mocked(api.updatePage).mockResolvedValue({ ...aboutDetail, title: "关于我们", version: 3 });
    vi.mocked(api.listPages).mockResolvedValue([{ ...aboutPage, title: "关于我们", version: 3 }]);
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));
    await waitFor(() => expect(api.updatePage).toHaveBeenCalledTimes(1));

    fireEvent.click(screen.getByRole("menuitem", { name: "独立页面" }));
    expect(await screen.findByText("关于我们")).toBeTruthy();
  });

  // 与文章对称：先行保存成功、状态切换失败时，列表也必须反映保存结果。
  it("页面撤回失败时，先行保存的结果仍会反映到列表", async () => {
    // aboutPage 是已发布状态，按钮是「撤回为草稿」，需要 page.unpublish（只影响本条用例）。
    state.permissions = ["page.create", "page.unpublish"];
    window.history.replaceState(null, "", paths.pages);
    vi.mocked(api.listPages).mockResolvedValue([aboutPage]);
    render(<App />);
    await screen.findByText("关于"); // 列表缓存先落地

    act(() => { navigate(paths.editPage(aboutPage.id)); });
    await screen.findByDisplayValue("关于");
    fireEvent.change(screen.getByLabelText("标题"), { target: { value: "关于我们" } });
    vi.mocked(api.updatePage).mockResolvedValue({ ...aboutDetail, title: "关于我们", version: 3 });
    vi.mocked(api.unpublishPage).mockRejectedValue(
      new ApiError(500, "撤回失败", "internal", "req-8"),
    );
    vi.mocked(api.listPages).mockResolvedValue([{ ...aboutPage, title: "关于我们", version: 3 }]);

    fireEvent.click(screen.getByRole("button", { name: "撤回为草稿" }));
    await waitFor(() => expect(api.updatePage).toHaveBeenCalledTimes(1));
    expect(api.unpublishPage).toHaveBeenCalledWith(aboutPage.id, 3);
    expect(await screen.findByText(/撤回失败（错误编号 req-8）/)).toBeTruthy();

    fireEvent.click(screen.getByRole("menuitem", { name: "独立页面" }));
    expect(await screen.findByText("关于我们")).toBeTruthy();
  });
});
