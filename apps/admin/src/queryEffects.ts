import type { QueryClient, QueryKey } from "@tanstack/react-query";
import { queryKeys as keys } from "./queryClient";

/**
 * 一次业务提交影响的读取视图。按查询族失效，覆盖已访问但当前未挂载的分页。
 * 必须在每一步写入成功后调用：后续发布、上传或账号刷新失败，不能撤销已提交的变化。
 * 表单基线仍由写响应维护，不用查询重取的结果覆盖正在编辑的输入。
 */
const effects = {
  post: [keys.posts(), keys.trashAll(), keys.tags(), keys.categories(), keys.series(), keys.mediaAll(), keys.commentsAll()],
  page: [keys.pages(), keys.pageTrashAll(), keys.mediaAll()],
  tag: [keys.tags(), keys.posts(), keys.trashAll()],
  category: [keys.categories(), keys.posts(), keys.trashAll()],
  series: [keys.series(), keys.posts(), keys.trashAll(), keys.mediaAll()],
  media: [keys.mediaAll()],
  profile: [keys.users(), keys.mediaAll()],
  site: [keys.siteSettings(), keys.mediaAll()],
  comments: [keys.commentsAll()],
} satisfies Record<string, readonly QueryKey[]>;

export async function invalidateAfterWrite(client: QueryClient, change: keyof typeof effects): Promise<void> {
  await Promise.all([...effects[change], keys.auditLogsAll()].map(queryKey =>
    client.invalidateQueries({ queryKey }),
  ));
}
