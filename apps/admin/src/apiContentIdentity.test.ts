import { afterEach, describe, expect, it, vi } from "vitest";
import { postsApi } from "./api/posts";
import { pagesApi } from "./api/pages";
import { jsonResponse, postResponse } from "../tests/httpFixtures";

const id = "0195c98a-6430-7000-8000-000000000001";

afterEach(() => vi.unstubAllGlobals());

describe("内容管理 API 的稳定身份契约", () => {
  // 编辑器测试 mock api；这里覆盖实际 HTTP 地址，防止适配器仍把 ID 发给 slug 路由。
  const calls: { resource: string; action: string; method: string; run: () => Promise<unknown> }[] = [
    { resource: "posts", action: "", method: "GET", run: () => postsApi.getPost(id) },
    { resource: "posts", action: "", method: "PATCH", run: () => postsApi.updatePost(id, { new_slug: "renamed", expected_version: 7 }) },
    { resource: "posts", action: "/publish", method: "POST", run: () => postsApi.publishPost(id, 7) },
    { resource: "posts", action: "/unpublish", method: "POST", run: () => postsApi.unpublishPost(id, 7) },
    { resource: "posts", action: "/trash", method: "POST", run: () => postsApi.trashPost(id, 7) },
    { resource: "posts", action: "/restore", method: "POST", run: () => postsApi.restorePost(id, 7) },
    { resource: "posts", action: "/purge", method: "POST", run: () => postsApi.purgePost(id, 7) },
    { resource: "pages", action: "", method: "GET", run: () => pagesApi.getPage(id) },
    { resource: "pages", action: "", method: "PATCH", run: () => pagesApi.updatePage(id, { new_slug: "renamed", expected_version: 7 }) },
    { resource: "pages", action: "/publish", method: "POST", run: () => pagesApi.publishPage(id, 7) },
    { resource: "pages", action: "/unpublish", method: "POST", run: () => pagesApi.unpublishPage(id, 7) },
    { resource: "pages", action: "/trash", method: "POST", run: () => pagesApi.trashPage(id, 7) },
  ];

  it.each(calls)("$method v1/$resource/{id}$action", async ({ resource, action, method, run }) => {
    const fetch = vi.fn().mockResolvedValue(action === "/purge" ? new Response(null, { status: 204 }) : jsonResponse(postResponse()));
    vi.stubGlobal("fetch", fetch);
    await run();
    const [path, init] = fetch.mock.calls[0] as [string, RequestInit];
    expect(path).toBe(`/api/admin/v1/${resource}/${id}${action}`);
    expect(init.method ?? "GET").toBe(method);
    if (method === "DELETE") {
      expect(JSON.parse(init.body as string)).toEqual({ expected_version: 7 });
    } else if (method !== "GET") {
      expect(JSON.parse(init.body as string)).toMatchObject({ expected_version: 7 });
    }
  });

  it.each([
    { path: "/posts", method: "GET", run: () => postsApi.listPosts() },
    { path: "/pages", method: "GET", run: () => pagesApi.listPages() },
    { path: "/posts?page=2&status=draft&visibility=private&author=a%26b", method: "GET", run: () => postsApi.listPosts({ page: 2, status: "draft", visibility: "private", author: "a&b" }) },
    { path: `/posts?page=1&q=body%26slug&scope=all&category_id=${id}`, method: "GET", run: () => postsApi.listPosts({ page: 1, q: "body&slug", scope: "all", category_id: id }) },
    { path: "/pages?page=3&status=published", method: "GET", run: () => pagesApi.listPages({ page: 3, status: "published" }) },
    { path: "/post-trash?page=2", method: "GET", run: () => postsApi.listTrash(2) },
    { path: "/post-trash?page=2&q=body&scope=all&author=disabled", method: "GET", run: () => postsApi.listTrash(2, "disabled", { q: "body", scope: "all" }) },
    { path: "/posts", method: "POST", run: () => postsApi.createPost({ title: "新文章", content: "正文", visibility: "public" }) },
    { path: "/pages", method: "POST", run: () => pagesApi.createPage({ title: "新页面", content: "正文", visibility: "public" }) },
  ])("列表和新建使用 v1 的 ID 契约：$method $path", async ({ path, method, run }) => {
    const fetch = vi.fn().mockResolvedValue(jsonResponse(method === "GET" ? { items: [{ ...postResponse(), author_username: "author" }], total: 1, page: 1, per_page: 20 } : postResponse()));
    vi.stubGlobal("fetch", fetch);
    await run();
    const [url, init] = fetch.mock.calls[0] as [string, RequestInit];
    expect(url).toBe(`/api/admin/v1${path}`);
    expect(init.method ?? "GET").toBe(method);
  });
});
