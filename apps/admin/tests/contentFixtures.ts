import type { ContentPage, PostDetail, PostSummary } from "../src/types";
export function contentPage<T>(items: T[], page = 1, total = items.length): ContentPage<T> {
  return { items, page, total, per_page: 20 };
}

export function postSummary(post: PostDetail, authorUsername = "author"): PostSummary {
  return { ...post, author_username: authorUsername };
}
