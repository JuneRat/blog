import type { ContentListFilter } from "../types";
export function contentQuery(
  filter: Partial<ContentListFilter> & { author?: string },
): string {
  const params = new URLSearchParams();
  if (filter.page !== undefined) params.set("page", String(filter.page));
  if (filter.status) params.set("status", filter.status);
  if (filter.visibility) params.set("visibility", filter.visibility);
  if (filter.author) params.set("author", filter.author);
  return params.size ? `?${params}` : "";
}
