import { QueryClient } from "@tanstack/react-query";
import { ApiError } from "./api";

/**
 * 服务端状态的查询键。集中定义，避免各屏各写一份字符串数组——
 * 失效（invalidate）写错键是这类库最常见、也最难发现的 bug。
 */
export const queryKeys = {
  posts: () => ["posts"] as const,
  pages: () => ["pages"] as const,
  trash: (page: number) => ["trash", page] as const,
  /**
   * 回收站**整族**前缀。
   *
   * 页码是键的一部分，写操作（恢复/永久删除）改变的是总数与其他页的内容，
   * 只失效当前页会留下兄弟页的陈旧数据。失效族用 `queryKeys.trashAll()`，
   * 取具体页用 `queryKeys.trash(page)`。
   */
  trashAll: () => ["trash"] as const,
  tags: () => ["tags"] as const,
  categories: () => ["categories"] as const,
  series: () => ["series"] as const,
  users: () => ["users"] as const,
  roles: () => ["roles"] as const,
  media: (page: number, trash = false) => ["media", page, trash] as const,
  /**
   * 媒体库**整族**前缀。
   *
   * 与回收站同理：上传/删除改变的是总数与其他页的内容，只失效当前页
   * （上传后只失效第 1 页）会在 30s staleTime 内留下兄弟页的陈旧列表。
   * 失效族用 `queryKeys.mediaAll()`，取具体页用 `queryKeys.media(page)`。
   */
  mediaAll: () => ["media"] as const,
  mediaUsage: (id: string) => ["media", "usage", id] as const,
  siteSettings: () => ["settings", "site"] as const,
  themeSettings: () => ["settings", "theme"] as const,
};

/**
 * 每个应用实例一个 QueryClient。
 *
 * 建在 `AdminProviders` 里（用 `useState` 惰性创建）而不是模块级单例：
 * 测试是同文件共用模块状态的，模块级缓存会让用例之间互相污染——
 * 上一条用例取到的数据被下一条当成「已有缓存」，于是请求不再发出、断言莫名通过。
 */
export function createQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: {
        /**
         * 只重试服务端故障（5xx）。
         *
         * 业务错误码重试没有意义，而且有害：`version_conflict` 重试会掩盖真实的并发冲突，
         * `forbidden`/`tag_in_use`/`not_found` 重试三次只是把错误提示推迟几秒。
         */
        retry: (count, error) => error instanceof ApiError && error.status >= 500 && count < 2,
        /**
         * 后台是单人/少人使用的管理界面：窗口重新聚焦时不自动重取。
         * 写操作后的新鲜度由 `invalidateQueries` 显式负责，不靠后台静默刷新。
         */
        refetchOnWindowFocus: false,
        staleTime: 30_000,
      },
      mutations: {
        // 写操作绝不自动重试：不知道服务端是否已应用，重试可能造成重复写入。
        retry: false,
      },
    },
  });
}
