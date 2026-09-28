import type { ContentPage } from "../src/types";
export function contentPage<T>(items: T[], page = 1, total = items.length): ContentPage<T> {
  return { items, page, total, per_page: 20 };
}
