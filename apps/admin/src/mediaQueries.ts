import { queryOptions } from "@tanstack/react-query";
import { mediaApi } from "./api";
import { queryKeys } from "./queryClient";

/** All media selectors share the library cache and cancellation semantics. */
export function mediaPageQuery(page: number, trash = false, q = "") {
  return queryOptions({
    queryKey: queryKeys.media(page, trash, q),
    queryFn: ({ signal }) => mediaApi.list(page, trash, signal, q),
  });
}
