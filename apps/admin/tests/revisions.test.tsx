import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AdminProviders } from "../src/providers";
import { PostEditScreen } from "../src/screens/PostEditScreen";
import { PageEditScreen } from "../src/screens/PageEditScreen";
import { pagesApi } from "../src/api/pages";
import { postsApi } from "../src/api/posts";
import { tagsApi, categoryApi, seriesApi } from "../src/api/taxonomy";
import { commentsApi } from "../src/api/comments";
import type { PageDetail, PostDetail } from "../src/types";

const auth = vi.hoisted(() => ({ user: "writer-1" }));
vi.mock("../src/auth", () => ({ useAuth: () => ({
  me: { user_id: auth.user, permissions: ["post.publish", "post.unpublish", "page.publish", "page.unpublish", "page.archive"] },
}) }));
vi.mock("../src/router", async (load) => ({ ...await load<typeof import("../src/router")>(), navigate: vi.fn() }));
vi.mock("../src/api/posts", async (load) => {
  const original = await load<typeof import("../src/api/posts")>();
  return { ...original, postsApi: { ...original.postsApi, getPost: vi.fn(), createPost: vi.fn(), updatePost: vi.fn(), unpublishPost: vi.fn(), archivePost: vi.fn(), publishPost: vi.fn(), revisions: vi.fn(), revision: vi.fn(), restoreRevision: vi.fn() } };
});
vi.mock("../src/api/pages", async (load) => {
  const original = await load<typeof import("../src/api/pages")>();
  return { ...original, pagesApi: { ...original.pagesApi, getPage: vi.fn(), createPage: vi.fn(), updatePage: vi.fn(), unpublishPage: vi.fn(), archivePage: vi.fn(), publishPage: vi.fn(), revisions: vi.fn(), revision: vi.fn(), restoreRevision: vi.fn() } };
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
function content() { return screen.getByLabelText("正文（Markdown）") as HTMLTextAreaElement; }
function deferred<T>() { let resolve!: (v: T) => void; const promise = new Promise<T>(done => { resolve = done; }); return { promise, resolve }; }
beforeEach(() => {
  vi.resetAllMocks(); localStorage.clear(); auth.user = "writer-1";
  vi.mocked(pagesApi.getPage).mockResolvedValue(page); vi.mocked(postsApi.getPost).mockResolvedValue(post);
  vi.mocked(tagsApi.listTags).mockResolvedValue([]); vi.mocked(categoryApi.list).mockResolvedValue([]); vi.mocked(seriesApi.list).mockResolvedValue([]);
  vi.mocked(commentsApi.policy).mockResolvedValue({ enabled: true, version: 1 });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("服务端编辑稿与历史恢复", () => {
  it.each(["post", "page"] as const)("%s 保存待发布修改不发布，显式发布使用新版本", async kind => {
    if (kind === "post") {
      vi.mocked(postsApi.getPost).mockResolvedValue({ ...post, status: "published" });
      vi.mocked(postsApi.updatePost).mockResolvedValue({ ...post, status: "published", content: "修改稿", version: 2, has_pending_changes: true });
      vi.mocked(postsApi.publishPost).mockResolvedValue({ ...post, status: "published", content: "修改稿", version: 3, has_pending_changes: false });
    } else {
      vi.mocked(pagesApi.getPage).mockResolvedValue({ ...page, status: "published" });
      vi.mocked(pagesApi.updatePage).mockResolvedValue({ ...page, status: "published", content: "修改稿", version: 2, has_pending_changes: true });
      vi.mocked(pagesApi.publishPage).mockResolvedValue({ ...page, status: "published", content: "修改稿", version: 3, has_pending_changes: false });
    }
    render(editor(kind)); await screen.findByDisplayValue(page.content);
    fireEvent.change(content(), { target: { value: "修改稿" } });
    fireEvent.click(screen.getByRole("button", { name: "保存修改草稿" }));
    await screen.findByText("有待发布修改，公开页面仍显示上次发布的内容。");
    expect(postsApi.publishPost).not.toHaveBeenCalled(); expect(pagesApi.publishPage).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "发布更新" }));
    await waitFor(() => expect(kind === "post" ? postsApi.publishPost : pagesApi.publishPage).toHaveBeenCalledWith(page.id, 2));
    await waitFor(() => expect(screen.queryByText("有待发布修改，公开页面仍显示上次发布的内容。")).toBeNull());
    expect(content().value).toBe("修改稿");
  });

  it.each(["post", "page"] as const)("%s 历史恢复有确认并保留请求期间的新输入", async kind => {
    const api = kind === "post" ? postsApi : pagesApi;
    vi.mocked(api.revisions).mockResolvedValue([{ id: "revision-1", version: 1, title: "旧标题", created_at: page.updated_at, actor_id: "writer-1" }]);
    vi.mocked(api.revision).mockResolvedValue({ slug: "writing", title: "旧标题", content: "旧正文", visibility: "public", excerpt: null, tag_ids: [], category_id: null, series: [], cover_media_id: null });
    const restoring = deferred<PostDetail>();
    if (kind === "post") vi.mocked(postsApi.restoreRevision).mockReturnValue(restoring.promise);
    else vi.mocked(pagesApi.restoreRevision).mockReturnValue(restoring.promise);
    render(editor(kind)); await screen.findByDisplayValue(page.content);
    fireEvent.click(screen.getByRole("button", { name: "历史版本" }));
    const selector = await screen.findByLabelText("选择历史版本");
    await waitFor(() => expect(api.revisions).toHaveBeenCalledWith(page.id));
    fireEvent.mouseDown(selector);
    fireEvent.click(await screen.findByTitle(/旧标题/));
    await screen.findByDisplayValue("旧正文");
    fireEvent.click(screen.getByRole("button", { name: "恢复到编辑稿" }));
    expect(api.restoreRevision).not.toHaveBeenCalled();
    const [confirmationTitle] = await screen.findAllByText("恢复这个历史版本到编辑稿？");
    const confirmation = confirmationTitle.closest(".ant-modal-confirm") as HTMLElement;
    fireEvent.click(within(confirmation).getByRole("button", { name: "恢复到编辑稿" }));
    await waitFor(() => expect(api.restoreRevision).toHaveBeenCalledWith(page.id, "revision-1", 1));
    fireEvent.change(content(), { target: { value: "请求期间新写的正文" } });
    await act(async () => restoring.resolve({ ...post, title: "旧标题", content: "旧正文", version: 2 }));
    await screen.findByText(/历史版本已恢复到编辑稿/);
    expect(content().value).toBe("请求期间新写的正文");
    expect(postsApi.publishPost).not.toHaveBeenCalled(); expect(pagesApi.publishPage).not.toHaveBeenCalled();
  });
});
