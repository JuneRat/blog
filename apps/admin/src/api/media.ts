import type * as Wire from "./generated";
import * as s from "./schemas";
import { json, request, requestBinary, requestEmpty } from "./client";
import type { MediaAsset, MediaPage, MediaUsageView } from "../types";

export const mediaApi = {
  list: (page = 1, trash = false, signal?: AbortSignal): Promise<MediaPage> =>
    request(
      s.mediaPage,
      `/api/admin/v1/media?page=${page}${trash ? "&trash=true" : ""}`,
      { signal },
    ),

  /** 资产详情与按内容阅读权限过滤的使用位置。 */
  detail: (id: string): Promise<MediaUsageView> =>
    request(s.mediaUsageView, `/api/admin/v1/media/${encodeURIComponent(id)}`),

  upload: (file: File): Promise<MediaAsset> =>
    requestBinary(
      s.mediaAsset,
      `/api/admin/v1/media?filename=${encodeURIComponent(file.name)}`,
      {
        method: "POST",
        headers: { "Content-Type": file.type || "application/octet-stream" },
        body: file,
      },
    ),

  /** 移入回收站；保留文件、链接和引用，成功返回 204。 */
  remove: (id: string, expectedVersion: number): Promise<void> =>
    requestEmpty(`/api/admin/v1/media/${encodeURIComponent(id)}`, {
      method: "DELETE",
      body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
    }),
  restore: (id: string, expectedVersion: number): Promise<void> =>
    requestEmpty(`/api/admin/v1/media/${encodeURIComponent(id)}/restore`, {
      method: "POST",
      body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
    }),
};
