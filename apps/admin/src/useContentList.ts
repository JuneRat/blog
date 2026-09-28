import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { queryKeys } from "./queryClient";
import type { ContentListFilter, ContentPage } from "./types";

export function useContentList<T>(kind: "posts" | "pages", fetchPage: (filter: ContentListFilter) => Promise<ContentPage<T>>) {
  const [filter, setFilter] = useState<ContentListFilter>({ page: 1 });
  const query = useQuery({
    queryKey: kind === "posts" ? queryKeys.postList(filter) : queryKeys.pageList(filter),
    queryFn: () => fetchPage(filter),
    // 翻页时保留行和服务端页码；改变筛选时不展示旧条件下的行。
    placeholderData: (previous, previousQuery) => {
      const prior = previousQuery?.queryKey[1];
      return prior?.status === filter.status && prior?.visibility === filter.visibility ? previous : undefined;
    },
  });

  useEffect(() => {
    const data = query.data;
    if (query.isPlaceholderData || query.isFetching || query.isError || !data || data.page !== filter.page) return;
    const last = Math.max(1, Math.ceil(data.total / data.per_page));
    if (filter.page > last) setFilter(current => ({ ...current, page: last }));
  }, [query.data, query.isPlaceholderData, query.isFetching, query.isError, filter.page]);

  return {
    query,
    filter,
    setFilter: (next: Omit<ContentListFilter, "page">) => setFilter({ ...next, page: 1 }),
    setPage: (page: number) => setFilter(current => ({ ...current, page })),
  };
}
