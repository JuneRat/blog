import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AdminProviders } from "../src/providers";
import { PostEditScreen } from "../src/screens/PostEditScreen";
import { PageEditScreen } from "../src/screens/PageEditScreen";
import { ApiError } from "../src/api/client";
import { pagesApi } from "../src/api/pages";
import { postsApi } from "../src/api/posts";
import { tagsApi, categoryApi, seriesApi } from "../src/api/taxonomy";
import { contentApi } from "../src/api/content";
import { commentsApi } from "../src/api/comments";
import { draftIdentity, draftKey, draftScope } from "../src/draftStorage";
import type { PageDetail, PostDetail } from "../src/types";

const auth = vi.hoisted(() => ({ user: "writer-1" }));
vi.mock("../src/auth", () => ({ useAuth: () => ({
  me: { user_id: auth.user, permissions: ["post.publish", "post.unpublish", "page.publish", "page.unpublish", "page.archive"] },
}) }));
vi.mock("../src/router", async (load) => ({ ...await load<typeof import("../src/router")>(), navigate: vi.fn() }));
vi.mock("../src/api/posts", async (load) => {
  const original = await load<typeof import("../src/api/posts")>();
  return { ...original, postsApi: { ...original.postsApi, getPost: vi.fn(), createPost: vi.fn(), updatePost: vi.fn(), unpublishPost: vi.fn(), archivePost: vi.fn() } };
});
vi.mock("../src/api/pages", async (load) => {
  const original = await load<typeof import("../src/api/pages")>();
  return { ...original, pagesApi: { ...original.pagesApi, getPage: vi.fn(), createPage: vi.fn(), updatePage: vi.fn(), unpublishPage: vi.fn(), archivePage: vi.fn() } };
});
vi.mock("../src/api/content", async (load) => {
  const original = await load<typeof import("../src/api/content")>();
  return { ...original, contentApi: { ...original.contentApi, previewContent: vi.fn() } };
});
vi.mock("../src/api/taxonomy", async (load) => {
  const original = await load<typeof import("../src/api/taxonomy")>();
  return { ...original, tagsApi: { ...original.tagsApi, listTags: vi.fn() }, categoryApi: { ...original.categoryApi, list: vi.fn() }, seriesApi: { ...original.seriesApi, list: vi.fn() } };
});
vi.mock("../src/api/comments", async (load) => {
  const original = await load<typeof import("../src/api/comments")>();
  return { ...original, commentsApi: { ...original.commentsApi, policy: vi.fn() } };
});
const page: PageDetail = { id: "writing-id", slug: "writing", title: "服务器标题", content: "服务器正文", status: "draft", visibility: "public", version: 1, published_at: null, updated_at: "2026-09-28T00:00:00Z" };
const post: PostDetail = { ...page, author_id: "writer-1", excerpt: "", tag_ids: [], category_id: null, series: [], cover_media_id: null, cover_url: null };
function editor(kind: "post" | "page", id: string | null = page.id) {
  return <AdminProviders>{kind === "post" ? <PostEditScreen id={id} /> : <PageEditScreen id={id} />}</AdminProviders>;
}
function ownKey(owner: string, id: string | null, kind: "post" | "page" = "page") { return draftKey(draftScope(owner, kind, id), draftIdentity()); }
function content() { return screen.getByLabelText("正文（Markdown）") as HTMLTextAreaElement; }
function deferred<T>() { let resolve!: (v: T) => void; const promise = new Promise<T>(done => { resolve = done; }); return { promise, resolve }; }
beforeEach(() => {
  vi.resetAllMocks(); localStorage.clear(); auth.user = "writer-1";
  vi.mocked(pagesApi.getPage).mockResolvedValue(page); vi.mocked(postsApi.getPost).mockResolvedValue(post);
  vi.mocked(tagsApi.listTags).mockResolvedValue([]); vi.mocked(categoryApi.list).mockResolvedValue([]); vi.mocked(seriesApi.list).mockResolvedValue([]);
  vi.mocked(commentsApi.policy).mockResolvedValue({ enabled: true, version: 1 });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("写作恢复与发布边界", () => {
  it.each(["post", "page"] as const)("%s 撤回不保存未公开编辑，失败也保留输入", async kind => {
    if (kind === "post") vi.mocked(postsApi.getPost).mockResolvedValue({ ...post, status: "published" });
    else vi.mocked(pagesApi.getPage).mockResolvedValue({ ...page, status: "published" });
    const unpublish = kind === "post" ? vi.mocked(postsApi.unpublishPost) : vi.mocked(pagesApi.unpublishPage);
    unpublish.mockRejectedValue(new ApiError(500, "撤回失败"));
    render(editor(kind)); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "撤回为草稿" }));
    await screen.findByText(/撤回失败/);
    expect(unpublish).toHaveBeenCalledWith(page.id, 1);
    expect(pagesApi.updatePage).not.toHaveBeenCalled(); expect(postsApi.updatePost).not.toHaveBeenCalled();
    expect(content().value).toBe("");
  });

  it.each(["post", "page"] as const)("%s 归档不保存本地编辑", async kind => {
    if (kind === "post") {
      vi.mocked(postsApi.getPost).mockResolvedValue({ ...post, status: "published" });
      vi.mocked(postsApi.archivePost).mockResolvedValue({ ...post, status: "archived", version: 2 });
    } else {
      vi.mocked(pagesApi.getPage).mockResolvedValue({ ...page, status: "published" });
      vi.mocked(pagesApi.archivePage).mockResolvedValue({ ...page, status: "archived", version: 2 });
    }
    render(editor(kind)); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "待完善的改写" } });
    fireEvent.click(screen.getByRole("button", { name: "归档" }));
    await screen.findByText(/状态已更新为已归档/);
    expect(pagesApi.updatePage).not.toHaveBeenCalled(); expect(postsApi.updatePost).not.toHaveBeenCalled();
    expect(content().value).toBe("待完善的改写");
  });

  it.each(["post", "page"] as const)("%s 显式恢复本机输入，保存后不复活旧副本", async kind => {
    const mounted = render(editor(kind)); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "浏览器关闭前的编辑" } });
    await screen.findByRole("button", { name: "删除本机副本" });
    mounted.unmount(); render(editor(kind));
    await screen.findByText("发现本机未保存的编辑");
    expect(content().value).toBe(page.content);
    fireEvent.click(screen.getByRole("button", { name: "恢复本机编辑" }));
    expect(content().value).toBe("浏览器关闭前的编辑");
    expect(postsApi.updatePost).not.toHaveBeenCalled(); expect(pagesApi.updatePage).not.toHaveBeenCalled();
    if (kind === "post") vi.mocked(postsApi.updatePost).mockResolvedValue({ ...post, content: "浏览器关闭前的编辑", version: 2 });
    else vi.mocked(pagesApi.updatePage).mockResolvedValue({ ...page, content: "浏览器关闭前的编辑", version: 2 });
    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    await screen.findByText("已保存。");
    expect(localStorage.getItem(ownKey(auth.user, page.id, kind))).toBeNull();
    cleanup(); render(editor(kind)); await screen.findByDisplayValue(page.content);
    expect(screen.queryByText("发现本机未保存的编辑")).toBeNull();
  });

  it("账号、内容 ID 与新建槽隔离，加载失败不覆盖其他副本", async () => {
    const mounted = render(editor("page")); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "账号一的草稿" } });
    await screen.findByRole("button", { name: "删除本机副本" });
    const key = ownKey(auth.user, page.id);
    const stored = localStorage.getItem(key);
    auth.user = "writer-2"; mounted.rerender(editor("page")); await screen.findByDisplayValue(page.content);
    expect(screen.queryByText("发现本机未保存的编辑")).toBeNull();
    expect(content().value).toBe(page.content);
    vi.mocked(pagesApi.getPage).mockRejectedValueOnce(new ApiError(404, "未找到"));
    mounted.rerender(editor("page", "missing")); await screen.findByText("页面未能加载。");
    expect(localStorage.getItem(key)).toBe(stored);
    mounted.rerender(editor("page", null)); await screen.findByText("新建页面");
    expect(content().value).toBe(""); expect(screen.queryByText("发现本机未保存的编辑")).toBeNull();
  });

  it.each(["pending", "failed"] as const)("新建页经过 %s 加载后复访仍可恢复，不清除副本", async state => {
    const pending = deferred<PageDetail>();
    const mounted = render(editor("page", null));
    fireEvent.change(content(), { target: { value: "未建档的新页面" } });
    await screen.findByRole("button", { name: "删除本机副本" });
    if (state === "pending") vi.mocked(pagesApi.getPage).mockReturnValue(pending.promise);
    else vi.mocked(pagesApi.getPage).mockRejectedValue(new ApiError(404, "未找到"));
    mounted.rerender(editor("page", "unavailable"));
    if (state === "failed") await screen.findByText("页面未能加载。");
    mounted.rerender(editor("page", null));
    await screen.findByText("发现本机未保存的编辑");
    expect(content().value).toBe("");
    fireEvent.click(screen.getByRole("button", { name: "恢复本机编辑" }));
    expect(content().value).toBe("未建档的新页面");
  });

  it("创建时清除新建槽，将请求期间输入保留在新 UUID 下", async () => {
    const pending = deferred<PageDetail>(); vi.mocked(pagesApi.createPage).mockReturnValue(pending.promise);
    const mounted = render(editor("page", null));
    fireEvent.change(content(), { target: { value: "首次保存" } });
    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    await waitFor(() => expect(pagesApi.createPage).toHaveBeenCalledTimes(1));
    fireEvent.change(content(), { target: { value: "保存期间继续写" } });
    await act(async () => pending.resolve({ ...page, content: "首次保存" }));
    expect(localStorage.getItem(ownKey("writer-1", null))).toBeNull();
    expect(JSON.parse(localStorage.getItem(ownKey("writer-1", "writing-id"))!).value.content).toBe("保存期间继续写");
    mounted.rerender(editor("page", page.id));
    expect(content().value).toBe("保存期间继续写");
    expect(screen.queryByText("发现本机未保存的编辑")).toBeNull();
  });

  it.each([ ["post", "route"], ["post", "account"], ["page", "route"], ["page", "account"] ] as const)("%s %s 切换后的旧保存响应不能回填或清除当前副本", async (kind, transition) => {
    const get = vi.mocked(kind === "post" ? postsApi.getPost : pagesApi.getPage);
    const update = vi.mocked(kind === "post" ? postsApi.updatePost : pagesApi.updatePage);
    const pending = deferred<PostDetail>(); update.mockReturnValue(pending.promise);
    const mounted = render(editor(kind)); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "旧会话提交" } });
    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    await waitFor(() => expect(update).toHaveBeenCalledTimes(1));
    const nextId = transition === "route" ? "another-id" : page.id;
    if (transition === "account") auth.user = "writer-2";
    get.mockResolvedValue({ ...post, id: nextId, content: "新会话正文" });
    mounted.rerender(editor(kind, nextId)); await screen.findByDisplayValue("新会话正文");
    fireEvent.change(content(), { target: { value: "新会话未保存编辑" } });
    await screen.findByRole("button", { name: "删除本机副本" });
    await act(async () => pending.resolve({ ...post, content: "旧会话提交", version: 2 }));
    expect(content().value).toBe("新会话未保存编辑");
    expect(JSON.parse(localStorage.getItem(ownKey(auth.user, nextId, kind))!).value.content).toBe("新会话未保存编辑");
    expect((screen.getByRole("button", { name: "保存草稿" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it.each(["post", "page"] as const)("%s 冲突覆盖绑定已展示版本，后续更新仍返回冲突并保留输入", async kind => {
    const get = vi.mocked(kind === "post" ? postsApi.getPost : pagesApi.getPage);
    const update = vi.mocked(kind === "post" ? postsApi.updatePost : pagesApi.updatePage);
    get.mockResolvedValueOnce(post).mockResolvedValueOnce({ ...post, content: "同事修改", version: 2 }).mockResolvedValue({ ...post, content: "同事再次修改", version: 3 });
    update.mockRejectedValue(new ApiError(409, "版本冲突", "version_conflict"));
    render(editor(kind)); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "我的修改" } });
    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    await screen.findByText("服务器版本 v2"); await screen.findByText("同事修改");
    fireEvent.click(screen.getByRole("button", { name: "仍然覆盖" }));
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));
    await waitFor(() => expect(update).toHaveBeenLastCalledWith(page.id, expect.objectContaining({ expected_version: 2, content: "我的修改" })));
    await screen.findByText("服务器版本 v3"); expect(content().value).toBe("我的修改");
  });

  it.each(["post", "page"] as const)("%s 旧版本本机恢复保留原提交前提，普通保存不能覆盖当前服务器版本", async kind => {
    const get = vi.mocked(kind === "post" ? postsApi.getPost : pagesApi.getPage);
    const update = vi.mocked(kind === "post" ? postsApi.updatePost : pagesApi.updatePage);
    const mounted = render(editor(kind)); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "基于 v1 的离线修改" } });
    await screen.findByRole("button", { name: "删除本机副本" }); mounted.unmount();
    get.mockResolvedValue({ ...post, content: "服务器 v2 正文", version: 2 });
    update.mockRejectedValue(new ApiError(409, "版本冲突", "version_conflict"));
    render(editor(kind)); await screen.findByText("发现本机未保存的编辑");
    fireEvent.click(screen.getByRole("button", { name: "恢复本机编辑" }));
    await screen.findByText("服务器版本 v2");
    expect(JSON.parse(localStorage.getItem(ownKey("writer-1", "writing-id", kind))!).baselineVersion).toBe(1);
    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    await waitFor(() => expect(update).toHaveBeenCalledWith(page.id, expect.objectContaining({ expected_version: 1, content: "基于 v1 的离线修改" })));
    expect(content().value).toBe("基于 v1 的离线修改");
  });

  it("保存期间禁止恢复或丢弃旧副本，避免响应为恢复输入换基线", async () => {
    const mounted = render(editor("page")); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "旧基线修改" } });
    await screen.findByRole("button", { name: "删除本机副本" }); mounted.unmount();
    // A real browser refresh has a new document writer; the recovery source is a previous-document slot.
    const previousKey = ownKey("writer-1", page.id);
    localStorage.setItem(`${previousKey}-previous`, localStorage.getItem(previousKey)!);
    localStorage.removeItem(previousKey);
    vi.mocked(pagesApi.getPage).mockResolvedValue({ ...page, content: "服务器新正文", version: 2 });
    const pending = deferred<PageDetail>(); vi.mocked(pagesApi.updatePage).mockReturnValue(pending.promise);
    render(editor("page")); await screen.findByText("发现本机未保存的编辑");
    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    await waitFor(() => expect(pagesApi.updatePage).toHaveBeenCalledTimes(1));
    const restore = screen.getByRole("button", { name: "恢复本机编辑" }) as HTMLButtonElement;
    expect(restore.disabled).toBe(true);
    expect((screen.getByRole("button", { name: "忽略此恢复副本" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(restore);
    expect(content().value).toBe("服务器新正文");
    await act(async () => pending.resolve({ ...page, content: "服务器新正文", version: 2 }));
    expect(content().value).toBe("服务器新正文");
  });

  it("删除本机副本不删除输入，后续新输入才重新保存", async () => {
    render(editor("page")); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "暂存正文" } });
    fireEvent.click(await screen.findByRole("button", { name: "删除本机副本" }));
    expect(content().value).toBe("暂存正文"); expect(localStorage.getItem(ownKey(auth.user, page.id))).toBeNull();
    fireEvent.change(content(), { target: { value: "下一次编辑" } });
    await screen.findByRole("button", { name: "删除本机副本" }); expect(JSON.parse(localStorage.getItem(ownKey(auth.user, page.id))!).value.content).toBe("下一次编辑");
  });

  it("预览未保存正文并忽略过期响应，不触发保存", async () => {
    const pending = deferred<{ content_html: string; head_html: string }>(); vi.mocked(contentApi.previewContent).mockReturnValueOnce(pending.promise).mockResolvedValue({ content_html: "<strong>新正文</strong>", head_html: "" });
    render(editor("page")); await screen.findByDisplayValue(page.content);
    fireEvent.click(screen.getByRole("button", { name: "预览正文" }));
    expect(contentApi.previewContent).toHaveBeenCalledWith(page.content);
    fireEvent.change(content(), { target: { value: "**新正文**" } });
    await act(async () => pending.resolve({ content_html: "<p>过期正文</p>", head_html: "" }));
    expect(screen.queryByLabelText("正文预览")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "预览正文" }));
    const preview = await screen.findByLabelText("正文预览") as HTMLIFrameElement;
    expect(preview.srcdoc).toContain("<main data-content-root><strong>新正文</strong></main>");
    expect(preview.getAttribute("sandbox")).toBe("allow-scripts");
    expect(pagesApi.updatePage).not.toHaveBeenCalled();
  });
});
