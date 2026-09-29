import { useEffect, useMemo } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { contentQuery } from "./api/contentQuery";
import { useAuth } from "./auth";
import { navigate, paths, useLocation } from "./router";
import type { ContentListFilter, ContentPage } from "./types";

export function readContentFilter(search: string): ContentListFilter {
  const params = new URLSearchParams(search);
  const page = Number(params.get("page") || 1);
  const filter: ContentListFilter = { page: Number.isSafeInteger(page) && page > 0 ? page : 1 };
  for (const key of ["status", "q", "author", "category_id"] as const) {
    const value = params.get(key)?.trim();
    if (value) filter[key] = value;
  }
  const visibility = params.get("visibility");
  if (visibility === "public" || visibility === "private") filter.visibility = visibility;
  const scope = params.get("scope");
  if (scope === "all" || scope === "mine") filter.scope = scope;
  return filter;
}

export function useContentList<T>(kind: "posts" | "pages" | "trash" | "page-trash", fetchPage: (filter: ContentListFilter) => Promise<ContentPage<T>>) {
  const client = useQueryClient();
  const { me } = useAuth();
  const userId = me?.user_id;
  const location = useLocation();
  const listPath = { posts: paths.list, pages: paths.pages, trash: paths.postTrash, "page-trash": paths.pageTrash }[kind];
  const active = new URL(location).pathname.replace(/\/$/, "") === listPath.replace(/\/$/, "");
  // URL supports reload and Back; per-login query cache restores a menu return from the editor.
  const filter = useMemo(() => {
    const search = new URL(location).search;
    return active && search ? readContentFilter(search) : client.getQueryData<ContentListFilter>(["list-filter", kind, userId]) ?? { page: 1 };
  }, [location, client, kind, userId, active]);
  const update = (next: ContentListFilter) => {
    client.setQueryData(["list-filter", kind, userId], next);
    navigate(`${listPath}${contentQuery(next)}`, { replace: true });
  };
  useEffect(() => {
    if (!active) return;
    client.setQueryData(["list-filter", kind, userId], filter);
    const url = new URL(location);
    const search = contentQuery(filter);
    if (!url.search && search !== "?page=1") {
      navigate(`${url.pathname}${search}`, { replace: true });
    }
  }, [client, kind, userId, filter, location, active]);
  const query = useQuery({
    queryKey: [kind, filter, userId],
    enabled: active,
    queryFn: () => fetchPage(filter),
    placeholderData: (previous, previousQuery) => {
      const prior = previousQuery?.queryKey[1] as ContentListFilter | undefined;
      return prior && previousQuery?.queryKey[2] === userId && contentQuery({ ...prior, page: undefined }) === contentQuery({ ...filter, page: undefined }) ? previous : undefined;
    },
  });
  useEffect(() => {
    const data = query.data;
    if (!active || query.isPlaceholderData || query.isFetching || query.isError || !data || data.page !== filter.page) return;
    const last = Math.max(1, Math.ceil(data.total / data.per_page));
    if (filter.page > last) update({ ...filter, page: last });
  }, [query.data, query.isPlaceholderData, query.isFetching, query.isError, filter, location, active]);
  return {
    query,
    filter,
    setFilter: (next: Omit<ContentListFilter, "page">) => update({ ...next, page: 1 }),
    setPage: (page: number) => update({ ...filter, page }),
  };
}
