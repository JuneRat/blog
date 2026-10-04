import { contentPage, postSummary } from "./contentFixtures";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { postsApi } from "../src/api/posts";
import { tagsApi, categoryApi, seriesApi } from "../src/api/taxonomy";
import { pagesApi } from "../src/api/pages";
import { commentsApi } from "../src/api/comments";
import { mediaApi } from "../src/api/media";
import { navigate, paths } from "../src/router";
import type { MediaAsset, PageDetail, PostDetail } from "../src/types";

vi.mock("../src/auth", () => ({
  useAuth: () => ({ status: "authenticated", me: { user_id: "me", permissions: [
    "post.create", "post.publish", "post.unpublish", "page.read", "page.edit",
    "media.read", "media.upload", "comment.moderate",
  ] } }),
}));
vi.mock("../src/api/posts", async (load) => {
  const original = await load<typeof import("../src/api/posts")>();
  return { ...original, postsApi: { ...original.postsApi, getPost: vi.fn(), updatePost: vi.fn(), publishPost: vi.fn(), unpublishPost: vi.fn(), listPosts: vi.fn() } };
});
vi.mock("../src/api/taxonomy", async (load) => {
  const original = await load<typeof import("../src/api/taxonomy")>();
  return { ...original, tagsApi: { ...original.tagsApi, listTags: vi.fn() }, categoryApi: { list: vi.fn() }, seriesApi: { list: vi.fn(), members: vi.fn() } };
});
vi.mock("../src/api/pages", async (load) => {
  const original = await load<typeof import("../src/api/pages")>();
  return { ...original, pagesApi: { ...original.pagesApi, getPage: vi.fn(), updatePage: vi.fn() } };
});
vi.mock("../src/api/comments", async (load) => {
  const original = await load<typeof import("../src/api/comments")>();
  return { ...original, commentsApi: { list: vi.fn(), policy: vi.fn() } };
});
vi.mock("../src/api/media", async (load) => {
  const original = await load<typeof import("../src/api/media")>();
  return { ...original, mediaApi: { list: vi.fn(), detail: vi.fn(), upload: vi.fn() } };
});

let post: PostDetail;
let page: PageDetail;
const picture: MediaAsset = {
  id: "image", original_name: "picture.png", mime: "image/png", byte_size: 128,
  width: 10, height: 10, deleted_at: null, version: 1, created_at: "2026-09-28T00:00:00Z",
  owner_id: "me", owner_display: "作者", url: "/media/image", reference_count: 0,
};
function count() { return post.status === "published" ? 1 : 0; }
function references() {
  return [{ kind: "post" as const, value: post }, { kind: "page" as const, value: page }]
    .filter(({ value }) => value.content.includes(picture.url))
    .map(({ kind, value }) => ({ kind, content_id: value.id, title: value.title, slug: value.slug,
      status: value.status, visibility: value.visibility, deleted: false, public: value.status === "published" }));
}
function media() { return { ...picture, reference_count: references().length }; }
function go(path: string) { act(() => navigate(path)); }
function change(label: string, value: string) { fireEvent.change(screen.getByLabelText(label), { target: { value } }); }
async function catalog(path: string, name: string, value: number) {
  go(path);
  const row = (await screen.findByRole("link", { name })).closest("tr")!;
  await waitFor(() => expect(within(row).getByRole("cell", { name: String(value) })).toBeTruthy());
}
async function usage() {
  go(paths.media);
  await screen.findByText(picture.original_name);
  fireEvent.click(screen.getByRole("button", { name: /查看使用位置|被 \d+ 处引用/ }));
  await screen.findByText(/的使用位置/);
}

beforeEach(() => {
  vi.resetAllMocks();
  window.history.replaceState(null, "", paths.media);
  post = { id: "post", slug: "old-post", title: "旧文章标题", content: "初始正文", excerpt: null,
    status: "draft", visibility: "public", version: 1, published_at: null, updated_at: "2026-09-28T00:00:00Z",
    author_id: "me", tag_ids: ["tag"], category_id: "category", series: [{ series_id: "series", position: 0 }],
    cover_media_id: null, cover_url: null };
  page = { id: "page", slug: "about", title: "关于页面", content: "页面正文", status: "draft",
    visibility: "public", version: 1, published_at: null, updated_at: "2026-09-28T00:00:00Z" };
  vi.mocked(postsApi.getPost).mockImplementation(async () => post);
  vi.mocked(postsApi.updatePost).mockImplementation(async (_id, body) => {
    post = { ...post, title: body.title ?? post.title, content: body.content ?? post.content, version: post.version + 1 };
    return post;
  });
  vi.mocked(postsApi.publishPost).mockImplementation(async () => {
    post = { ...post, status: "published", version: post.version + 1 };
    return post;
  });
  vi.mocked(postsApi.unpublishPost).mockImplementation(async () => {
    post = { ...post, status: "draft", version: post.version + 1 };
    return post;
  });
  vi.mocked(postsApi.listPosts).mockImplementation(async () => contentPage([postSummary(post)]));
  vi.mocked(tagsApi.listTags).mockImplementation(async () => [
    { id: "tag", name: "技术标签", slug: "tech", version: 1, public_post_count: count() },
  ]);
  vi.mocked(categoryApi.list).mockImplementation(async () => [
    { id: "category", name: "开发分类", slug: "dev", version: 1, parent_id: null, description: null, pub_post_count: count() },
  ]);
  vi.mocked(seriesApi.list).mockImplementation(async () => [
    { id: "series", name: "教程系列", slug: "guide", version: 1, description: null,
      post_count: 1, pub_post_count: count(), cover_media_id: null, cover_url: null },
  ]);
  vi.mocked(seriesApi.members).mockImplementation(async () => [{ ...post, deleted: false, position: 0 }]);
  vi.mocked(commentsApi.policy).mockResolvedValue({ enabled: true, version: 1 });
  vi.mocked(commentsApi.list).mockImplementation(async () => ({ items: [{
    id: "comment", post_id: post.id, post_slug: post.slug, post_title: post.title, status: "pending", version: 1,
    parent_id: null, root_id: null, parent_nickname: null, author_email: null, ip_address: null, moderation_reason: "all_comments",
    content_html: "<p>读者留言</p>", body: "读者留言", nickname: "读者", is_author: false, created_at: "2026-09-28T00:00:00Z",
  }], total: 1, page: 1, per_page: 20, enabled: true }));
  vi.mocked(mediaApi.list).mockImplementation(async () => ({ items: [media()], total: 1, page: 1, per_page: 24 }));
  vi.mocked(mediaApi.detail).mockImplementation(async () => ({ media: media(), references: references(), hidden_references: 0 }));
  vi.mocked(pagesApi.getPage).mockImplementation(async () => page);
  vi.mocked(pagesApi.updatePage).mockImplementation(async (_id, body) => {
    page = { ...page, title: body.title ?? page.title, content: body.content ?? page.content, version: page.version + 1 };
    return page;
  });
});
afterEach(cleanup);

describe("写入后跨屏一致性（先访问旧缓存，再提交，再返回）", () => {
  it("文章保存、发布与撤回同步目录统计、系列成员、评论标题和媒体引用", async () => {
    render(<App />);
    await usage();
    await catalog(paths.tags, "技术标签", 0);
    await catalog(paths.categories, "开发分类", 0);
    go(paths.series);
    await screen.findByText(/0\/1 篇公开/);
    await screen.findByRole("link", { name: post.title });
    go(paths.comments);
    await screen.findByRole("button", { name: post.title });

    go(paths.editPost(post.id));
    await screen.findByDisplayValue(post.title);
    change("标题", "更新后的文章标题");
    change("正文（Markdown）", "加入图片 ![图片](/media/image)");
    // 发布先保存，再更新状态；两次成功都必须让关联读取失效。
    fireEvent.click(screen.getByRole("button", { name: "发布" }));
    await screen.findByText("状态已更新为已发布。");
    await usage();
    await screen.findByText("被 1 处引用");
    await screen.findByRole("button", { name: /文章：更新后的文章标题/ });
    await screen.findByText(/已发布；公开可读/);
    await catalog(paths.tags, "技术标签", 1);
    await catalog(paths.categories, "开发分类", 1);
    go(paths.series);
    await screen.findByText(/1\/1 篇公开/);
    await screen.findByRole("link", { name: "更新后的文章标题" });
    go(paths.comments);
    await screen.findByRole("button", { name: "更新后的文章标题" });
    expect(screen.queryByRole("button", { name: "旧文章标题" })).toBeNull();

    go(paths.editPost(post.id));
    await screen.findByDisplayValue(post.title);
    fireEvent.click(screen.getByRole("button", { name: "撤回为草稿" }));
    await screen.findByText("状态已更新为草稿。");
    await catalog(paths.tags, "技术标签", 0);
    await catalog(paths.categories, "开发分类", 0);
    go(paths.series);
    await screen.findByText(/0\/1 篇公开/);
    await usage();
    await screen.findByText(/草稿；不公开/);
  }, 30_000);

  it("页面移除图片后，媒体列表与已看过的使用位置一起刷新", async () => {
    page.content = "![图片](/media/image)";
    render(<App />);
    await usage();
    await screen.findByText("被 1 处引用");
    await screen.findByRole("button", { name: /页面：关于页面/ });
    go(paths.editPage(page.id));
    await screen.findByDisplayValue(page.title);
    change("正文（Markdown）", "移除图片后只保留文字");
    fireEvent.click(screen.getByRole("button", { name: /保存草稿|保存预约内容|保存修改草稿/ }));
    await screen.findByText("已保存。");
    await usage();
    await waitFor(() => expect(screen.queryByRole("button", { name: /页面：关于页面/ })).toBeNull());
    expect(screen.queryByText("被 1 处引用")).toBeNull();
  });

  it("正文粘贴上传也刷新已看过的媒体库，即使没有保存正文", async () => {
    let uploaded = false;
    vi.mocked(mediaApi.upload).mockImplementation(async () => { uploaded = true; return { ...picture, id: "new-image", original_name: "pasted.png", url: "/media/new-image" }; });
    vi.mocked(mediaApi.list).mockImplementation(async () => ({ items: uploaded ? [
      { ...picture, id: "new-image", original_name: "pasted.png", url: "/media/new-image" }, picture,
    ] : [picture], total: uploaded ? 2 : 1, page: 1, per_page: 24 }));
    render(<App />);
    await screen.findByText(picture.original_name);
    go(paths.editPost(post.id));
    await screen.findByDisplayValue(post.title);
    const body = screen.getByLabelText("正文（Markdown）") as HTMLTextAreaElement;
    fireEvent.paste(body, { clipboardData: { files: [new File(["png"], "pasted.png", { type: "image/png" })], types: ["Files"] } });
    await waitFor(() => expect(body.value).toContain("/media/new-image"));
    // 清除本地正文改动，资产仍然已经上传，不依赖文章保存来更新媒体缓存。
    change("正文（Markdown）", post.content);
    go(paths.media);
    await screen.findByText("pasted.png");
    expect(postsApi.updatePost).not.toHaveBeenCalled();
  });
});
