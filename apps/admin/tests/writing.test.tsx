import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AdminProviders } from "../src/providers";
import { PostEditScreen } from "../src/screens/PostEditScreen";
import { PageEditScreen } from "../src/screens/PageEditScreen";
import { ApiError, api, categoryApi, commentsApi, seriesApi } from "../src/api";
import { draftIdentity, draftKey, draftScope } from "../src/draftStorage";
import type { PageDetail, PostDetail } from "../src/types";

const auth = vi.hoisted(() => ({ user: "writer-1" }));
vi.mock("../src/auth", () => ({ useAuth: () => ({
  me: { user_id: auth.user, permissions: ["post.publish", "post.unpublish", "page.publish", "page.unpublish", "page.archive"] },
}) }));
vi.mock("../src/router", async (load) => ({ ...await load<typeof import("../src/router")>(), navigate: vi.fn() }));
vi.mock("../src/api", async (load) => {
  const actual = await load<typeof import("../src/api")>();
  return { ...actual, api: { ...actual.api,
    getPost: vi.fn(), getPage: vi.fn(), createPost: vi.fn(), createPage: vi.fn(), updatePost: vi.fn(), updatePage: vi.fn(),
    unpublishPost: vi.fn(), unpublishPage: vi.fn(), archivePost: vi.fn(), archivePage: vi.fn(), previewContent: vi.fn(), listTags: vi.fn(),
  }, categoryApi: { ...actual.categoryApi, list: vi.fn() }, seriesApi: { ...actual.seriesApi, list: vi.fn() },
    commentsApi: { ...actual.commentsApi, policy: vi.fn() } };
});
const page: PageDetail = { id: "writing-id", slug: "writing", title: "服务器标题", content: "服务器正文", status: "draft", visibility: "public", version: 1, published_at: null, updated_at: "2026-09-28T00:00:00Z" };
const post: PostDetail = { ...page, author_id: "writer-1", excerpt: "", tag_ids: [], category_id: null, series: [], cover_media_id: null, cover_url: null };
function editor(kind: "post" | "page", id: string | null = page.id) {
  return <AdminProviders>{kind === "post" ? <PostEditScreen id={id} /> : <PageEditScreen id={id} />}</AdminProviders>;
}
function ownKey(owner: string, id: string | null) { return draftKey(draftScope(owner, "page", id), draftIdentity()); }
function content() { return screen.getByLabelText("正文（Markdown）") as HTMLTextAreaElement; }
function deferred<T>() { let resolve!: (v: T) => void; const promise = new Promise<T>(done => { resolve = done; }); return { promise, resolve }; }
beforeEach(() => {
  vi.resetAllMocks(); localStorage.clear(); auth.user = "writer-1";
  vi.mocked(api.getPage).mockResolvedValue(page); vi.mocked(api.getPost).mockResolvedValue(post);
  vi.mocked(api.listTags).mockResolvedValue([]); vi.mocked(categoryApi.list).mockResolvedValue([]); vi.mocked(seriesApi.list).mockResolvedValue([]);
  vi.mocked(commentsApi.policy).mockResolvedValue({ enabled: true, version: 1 });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("写作恢复与发布边界", () => {
  it.each(["post", "page"] as const)("%s 撤回不保存未公开编辑，失败也保留输入", async kind => {
    if (kind === "post") vi.mocked(api.getPost).mockResolvedValue({ ...post, status: "published" });
    else vi.mocked(api.getPage).mockResolvedValue({ ...page, status: "published" });
    const unpublish = kind === "post" ? vi.mocked(api.unpublishPost) : vi.mocked(api.unpublishPage);
    unpublish.mockRejectedValue(new ApiError(500, "撤回失败"));
    render(editor(kind)); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "撤回为草稿" }));
    await screen.findByText(/撤回失败/);
    expect(unpublish).toHaveBeenCalledWith(page.id, 1);
    expect(api.updatePage).not.toHaveBeenCalled(); expect(api.updatePost).not.toHaveBeenCalled();
    expect(content().value).toBe("");
  });

  it.each(["post", "page"] as const)("%s 归档不保存本地编辑", async kind => {
    if (kind === "post") {
      vi.mocked(api.getPost).mockResolvedValue({ ...post, status: "published" });
      vi.mocked(api.archivePost).mockResolvedValue({ ...post, status: "archived", version: 2 });
    } else {
      vi.mocked(api.getPage).mockResolvedValue({ ...page, status: "published" });
      vi.mocked(api.archivePage).mockResolvedValue({ ...page, status: "archived", version: 2 });
    }
    render(editor(kind)); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "待完善的改写" } });
    fireEvent.click(screen.getByRole("button", { name: "归档" }));
    await screen.findByText(/状态已更新为已归档/);
    expect(api.updatePage).not.toHaveBeenCalled(); expect(api.updatePost).not.toHaveBeenCalled();
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
    expect(api.updatePost).not.toHaveBeenCalled(); expect(api.updatePage).not.toHaveBeenCalled();
    if (kind === "post") vi.mocked(api.updatePost).mockResolvedValue({ ...post, content: "浏览器关闭前的编辑", version: 2 });
    else vi.mocked(api.updatePage).mockResolvedValue({ ...page, content: "浏览器关闭前的编辑", version: 2 });
    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    await screen.findByText("已保存。");
    expect(localStorage.length).toBe(0);
    cleanup(); render(editor(kind)); await screen.findByDisplayValue(page.content);
    expect(screen.queryByText("发现本机未保存的编辑")).toBeNull();
  });

  it("账号、内容 ID 与新建槽隔离，加载失败不覆盖其他副本", async () => {
    const mounted = render(editor("page")); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "账号一的草稿" } });
    await screen.findByRole("button", { name: "删除本机副本" });
    const stored = localStorage.getItem(localStorage.key(0)!);
    auth.user = "writer-2"; mounted.rerender(editor("page")); await screen.findByDisplayValue(page.content);
    expect(screen.queryByText("发现本机未保存的编辑")).toBeNull();
    expect(content().value).toBe(page.content);
    vi.mocked(api.getPage).mockRejectedValueOnce(new ApiError(404, "未找到"));
    mounted.rerender(editor("page", "missing")); await screen.findByText("页面未能加载。");
    expect(localStorage.length).toBe(1); expect(localStorage.getItem(localStorage.key(0)!)).toBe(stored);
    mounted.rerender(editor("page", null)); await screen.findByText("新建页面");
    expect(content().value).toBe(""); expect(screen.queryByText("发现本机未保存的编辑")).toBeNull();
  });

  it.each(["pending", "failed"] as const)("新建页经过 %s 加载后复访仍可恢复，不清除副本", async state => {
    const pending = deferred<PageDetail>();
    const mounted = render(editor("page", null));
    fireEvent.change(content(), { target: { value: "未建档的新页面" } });
    await screen.findByRole("button", { name: "删除本机副本" });
    if (state === "pending") vi.mocked(api.getPage).mockReturnValue(pending.promise);
    else vi.mocked(api.getPage).mockRejectedValue(new ApiError(404, "未找到"));
    mounted.rerender(editor("page", "unavailable"));
    if (state === "failed") await screen.findByText("页面未能加载。");
    mounted.rerender(editor("page", null));
    await screen.findByText("发现本机未保存的编辑");
    expect(content().value).toBe("");
    fireEvent.click(screen.getByRole("button", { name: "恢复本机编辑" }));
    expect(content().value).toBe("未建档的新页面");
  });

  it("创建时清除新建槽，将请求期间输入保留在新 UUID 下", async () => {
    const pending = deferred<PageDetail>(); vi.mocked(api.createPage).mockReturnValue(pending.promise);
    const mounted = render(editor("page", null));
    fireEvent.change(content(), { target: { value: "首次保存" } });
    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    await waitFor(() => expect(api.createPage).toHaveBeenCalledTimes(1));
    fireEvent.change(content(), { target: { value: "保存期间继续写" } });
    await act(async () => pending.resolve({ ...page, content: "首次保存" }));
    expect(localStorage.getItem(ownKey("writer-1", null))).toBeNull();
    expect(JSON.parse(localStorage.getItem(ownKey("writer-1", "writing-id"))!).value.content).toBe("保存期间继续写");
    mounted.rerender(editor("page", page.id));
    expect(content().value).toBe("保存期间继续写");
    expect(screen.queryByText("发现本机未保存的编辑")).toBeNull();
  });

  it.each(["route", "account"] as const)("%s 切换后的旧保存响应不能回填或清除当前副本", async transition => {
    const pending = deferred<PageDetail>(); vi.mocked(api.updatePage).mockReturnValue(pending.promise);
    const mounted = render(editor("page")); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "旧会话提交" } });
    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    await waitFor(() => expect(api.updatePage).toHaveBeenCalledTimes(1));
    const nextId = transition === "route" ? "another-id" : page.id;
    if (transition === "account") auth.user = "writer-2";
    vi.mocked(api.getPage).mockResolvedValue({ ...page, id: nextId, content: "新会话正文" });
    mounted.rerender(editor("page", nextId)); await screen.findByDisplayValue("新会话正文");
    fireEvent.change(content(), { target: { value: "新会话未保存编辑" } });
    await screen.findByRole("button", { name: "删除本机副本" });
    await act(async () => pending.resolve({ ...page, content: "旧会话提交", version: 2 }));
    expect(content().value).toBe("新会话未保存编辑");
    expect(JSON.parse(localStorage.getItem(ownKey(auth.user, nextId))!).value.content).toBe("新会话未保存编辑");
    expect((screen.getByRole("button", { name: "保存草稿" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("冲突覆盖绑定已展示版本，后续更新仍返回冲突并保留输入", async () => {
    vi.mocked(api.getPage).mockResolvedValueOnce(page).mockResolvedValueOnce({ ...page, content: "同事修改", version: 2 }).mockResolvedValue({ ...page, content: "同事再次修改", version: 3 });
    vi.mocked(api.updatePage).mockRejectedValue(new ApiError(409, "版本冲突", "version_conflict"));
    render(editor("page")); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "我的修改" } });
    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    await screen.findByText("服务器版本 v2"); await screen.findByText("同事修改");
    fireEvent.click(screen.getByRole("button", { name: "仍然覆盖" }));
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));
    await waitFor(() => expect(api.updatePage).toHaveBeenLastCalledWith(page.id, expect.objectContaining({ expected_version: 2, content: "我的修改" })));
    await screen.findByText("服务器版本 v3"); expect(content().value).toBe("我的修改");
    expect(api.getPage).toHaveBeenCalledTimes(3);
  });

  it("旧版本本机恢复保留原提交前提，普通保存不能覆盖当前服务器版本", async () => {
    const mounted = render(editor("page")); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "基于 v1 的离线修改" } });
    await screen.findByRole("button", { name: "删除本机副本" }); mounted.unmount();
    vi.mocked(api.getPage).mockResolvedValue({ ...page, content: "服务器 v2 正文", version: 2 });
    vi.mocked(api.updatePage).mockRejectedValue(new ApiError(409, "版本冲突", "version_conflict"));
    render(editor("page")); await screen.findByText("发现本机未保存的编辑");
    fireEvent.click(screen.getByRole("button", { name: "恢复本机编辑" }));
    await screen.findByText("服务器版本 v2");
    expect(JSON.parse(localStorage.getItem(ownKey("writer-1", "writing-id"))!).baselineVersion).toBe(1);
    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    await waitFor(() => expect(api.updatePage).toHaveBeenCalledWith(page.id, expect.objectContaining({ expected_version: 1, content: "基于 v1 的离线修改" })));
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
    vi.mocked(api.getPage).mockResolvedValue({ ...page, content: "服务器新正文", version: 2 });
    const pending = deferred<PageDetail>(); vi.mocked(api.updatePage).mockReturnValue(pending.promise);
    render(editor("page")); await screen.findByText("发现本机未保存的编辑");
    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    await waitFor(() => expect(api.updatePage).toHaveBeenCalledTimes(1));
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
    expect(content().value).toBe("暂存正文"); expect(localStorage.length).toBe(0);
    fireEvent.change(content(), { target: { value: "下一次编辑" } });
    await screen.findByRole("button", { name: "删除本机副本" }); expect(localStorage.length).toBe(1);
  });

  it("预览未保存正文并忽略过期响应，不触发保存", async () => {
    const pending = deferred<{ content_html: string }>(); vi.mocked(api.previewContent).mockReturnValueOnce(pending.promise).mockResolvedValue({ content_html: "<strong>新正文</strong>" });
    render(editor("page")); await screen.findByDisplayValue(page.content);
    fireEvent.click(screen.getByRole("button", { name: "预览正文" }));
    expect(api.previewContent).toHaveBeenCalledWith(page.content);
    fireEvent.change(content(), { target: { value: "**新正文**" } });
    await act(async () => pending.resolve({ content_html: "<p>过期正文</p>" }));
    expect(screen.queryByLabelText("正文预览")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "预览正文" }));
    expect((await screen.findByLabelText("正文预览")).innerHTML).toBe("<strong>新正文</strong>");
    expect(api.updatePage).not.toHaveBeenCalled();
  });
});
