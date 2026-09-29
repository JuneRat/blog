import type { ContentListFilter } from "../types";
export function contentQuery(
  filter: Partial<ContentListFilter> & { author?: string },
): string {
  const params = new URLSearchParams();
  if (filter.page !== undefined) params.set("page", String(filter.page));
  if (filter.status) params.set("status", filter.status);
  if (filter.visibility) params.set("visibility", filter.visibility);
  if (filter.q) params.set("q", filter.q);
  if (filter.scope) params.set("scope", filter.scope);
  if (filter.category_id) params.set("category_id", filter.category_id);
  if (filter.author) params.set("author", filter.author);
  return params.size ? `?${params}` : "";
}
