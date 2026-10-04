import type { Me, PostDetail } from "../src/types";

export function meResponse(overrides: Partial<Me> = {}): Me {
  return {
    user_id: "me", username: "author", display_name: null, bio: null, version: 1,
    avatar_media_id: null, avatar_url: null, time_zone: "UTC", permissions: [],
    csrf_token: "csrf", channel: "session",
    ...overrides,
  };
}

export function postResponse(overrides: Partial<PostDetail> = {}): PostDetail {
  return {
    id: "0195c98a-6430-7000-8000-000000000001",
    slug: "first",
    title: "文章",
    status: "draft",
    visibility: "public",
    version: 1,
    published_at: null,
    updated_at: "2026-09-29T00:00:00Z",
    author_id: "author-1",
    tag_ids: [],
    category_id: null,
    series: [],
    cover_media_id: null,
    cover_url: null,
    excerpt: null,
    content: "正文",
    ...overrides,
  };
}
export function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "Content-Type": "application/json",
      "x-request-id": "request-1",
    },
  });
}
