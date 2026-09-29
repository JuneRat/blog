import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "./api";
import { jsonResponse, postResponse } from "../tests/httpFixtures";

const id = "0195c98a-6430-7000-8000-000000000001";

afterEach(() => vi.unstubAllGlobals());

describe("内容管理 API 的稳定身份契约", () => {
  // 编辑器测试 mock api；这里覆盖实际 HTTP 地址，防止适配器仍把 ID 发给 slug 路由。
  const calls: { resource: string; action: string; method: string; run: () => Promise<unknown> }[] = [
    { resource: "posts", action: "", method: "GET", run: () => api.getPost(id) },
    { resource: "posts", action: "", method: "PATCH", run: () => api.updatePost(id, { new_slug: "renamed", expected_version: 7 }) },
    { resource: "posts", action: "/publish", method: "POST", run: () => api.publishPost(id, 7) },
    { resource: "posts", action: "/unpublish", method: "POST", run: () => api.unpublishPost(id, 7) },
    { resource: "posts", action: "/trash", method: "POST", run: () => api.trashPost(id, 7) },
    { resource: "posts", action: "/restore", method: "POST", run: () => api.restorePost(id, 7) },
    { resource: "posts", action: "/purge", method: "POST", run: () => api.purgePost(id, 7) },
    { resource: "pages", action: "", method: "GET", run: () => api.getPage(id) },
    { resource: "pages", action: "", method: "PATCH", run: () => api.updatePage(id, { new_slug: "renamed", expected_version: 7 }) },
    { resource: "pages", action: "/publish", method: "POST", run: () => api.publishPage(id, 7) },
    { resource: "pages", action: "/unpublish", method: "POST", run: () => api.unpublishPage(id, 7) },
    { resource: "pages", action: "/trash", method: "POST", run: () => api.trashPage(id, 7) },
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
    { path: "/posts", method: "GET", run: () => api.listPosts() },
    { path: "/pages", method: "GET", run: () => api.listPages() },
    { path: "/posts?page=2&status=draft&visibility=private&author=a%26b", method: "GET", run: () => api.listPosts({ page: 2, status: "draft", visibility: "private", author: "a&b" }) },
    { path: `/posts?page=1&q=body%26slug&scope=all&category_id=${id}`, method: "GET", run: () => api.listPosts({ page: 1, q: "body&slug", scope: "all", category_id: id }) },
    { path: "/pages?page=3&status=published", method: "GET", run: () => api.listPages({ page: 3, status: "published" }) },
    { path: "/post-trash?page=2", method: "GET", run: () => api.listTrash(2) },
    { path: "/post-trash?page=2&q=body&scope=all&author=disabled", method: "GET", run: () => api.listTrash(2, "disabled", { q: "body", scope: "all" }) },
    { path: "/posts", method: "POST", run: () => api.createPost({ title: "新文章", content: "正文", visibility: "public" }) },
    { path: "/pages", method: "POST", run: () => api.createPage({ title: "新页面", content: "正文", visibility: "public" }) },
  ])("列表和新建使用 v1 的 ID 契约：$method $path", async ({ path, method, run }) => {
    const fetch = vi.fn().mockResolvedValue(jsonResponse(method === "GET" ? { items: [{ ...postResponse(), author_username: "author" }], total: 1, page: 1, per_page: 20 } : postResponse()));
    vi.stubGlobal("fetch", fetch);
    await run();
    const [url, init] = fetch.mock.calls[0] as [string, RequestInit];
    expect(url).toBe(`/api/admin/v1${path}`);
    expect(init.method ?? "GET").toBe(method);
  });
});
