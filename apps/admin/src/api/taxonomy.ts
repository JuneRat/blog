import type * as Wire from "./generated";
import * as s from "./schemas";
import { json, request, requestEmpty } from "./client";
import type {
  SeriesSummary,
  CategorySummary,
  SeriesMemberRow,
  TagSummary,
} from "../types";
import type {
  CreateTagInput,
  RenameTagInput,
  CreateCategoryInput,
  UpdateCategoryInput,
  CreateSeriesInput,
  UpdateSeriesInput,
} from "./generated";

export const tagsApi = {
  listTags: (): Promise<TagSummary[]> =>
    request(s.array(s.tagSummary), "/api/admin/v1/tags"),

  createTag: (input: CreateTagInput): Promise<TagSummary> =>
    request(s.tagSummary, "/api/admin/v1/tags", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  renameTag: (slug: string, input: RenameTagInput): Promise<TagSummary> =>
    request(s.tagSummary, `/api/admin/v1/tags/${encodeURIComponent(slug)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  deleteTag: (slug: string, expectedVersion?: number): Promise<void> =>
    requestEmpty(`/api/admin/v1/tags/${encodeURIComponent(slug)}`, {
      method: "DELETE",
      body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
    }),
};

export const categoryApi = {
  list: (): Promise<CategorySummary[]> =>
    request(s.array(s.categorySummary), "/api/admin/v1/categories"),

  /** 创建。slug 冲突是 409 conflict；slug 创建后不可改。 */
  create: (input: CreateCategoryInput): Promise<CategorySummary> =>
    request(s.categorySummary, "/api/admin/v1/categories", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  /** 更新（改名/描述/移动父节点）。移动成环是 400 invalid_request。 */
  update: (
    slug: string,
    input: UpdateCategoryInput,
  ): Promise<CategorySummary> =>
    request(
      s.categorySummary,
      `/api/admin/v1/categories/${encodeURIComponent(slug)}`,
      {
        method: "PATCH",
        body: JSON.stringify(input),
      },
    ),

  /** 删除：被文章引用或仍有子分类时 409 category_in_use。 */
  remove: (slug: string, expectedVersion?: number): Promise<void> =>
    requestEmpty(`/api/admin/v1/categories/${encodeURIComponent(slug)}`, {
      method: "DELETE",
      body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
    }),
};

export const seriesApi = {
  list: (): Promise<SeriesSummary[]> =>
    request(s.array(s.seriesSummary), "/api/admin/v1/series"),

  create: (input: CreateSeriesInput): Promise<SeriesSummary> =>
    request(s.seriesSummary, "/api/admin/v1/series", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  update: (slug: string, input: UpdateSeriesInput): Promise<SeriesSummary> =>
    request(
      s.seriesSummary,
      `/api/admin/v1/series/${encodeURIComponent(slug)}`,
      {
        method: "PATCH",
        body: JSON.stringify(input),
      },
    ),

  /** 删除系列并解除成员关联，文章保留。 */
  remove: (slug: string, expectedVersion?: number): Promise<void> =>
    requestEmpty(`/api/admin/v1/series/${encodeURIComponent(slug)}`, {
      method: "DELETE",
      body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
    }),

  /** 管理目录：系列全部成员（含他人草稿/私密）；需 series.manage。 */
  members: (slug: string): Promise<SeriesMemberRow[]> =>
    request(
      s.array(s.seriesMemberRow),
      `/api/admin/v1/series/${encodeURIComponent(slug)}/members`,
    ),

  /** 整体重排（完整排列 + series 版本前提；改他人文章需 any 权限）。 */
  reorder: (
    slug: string,
    orderedPostIds: string[],
    expectedSeriesVersion?: number,
  ): Promise<{ series_version: number; ordered_post_ids: string[] }> =>
    request(
      s.reorderSeriesResult,
      `/api/admin/v1/series/${encodeURIComponent(slug)}/reorder`,
      {
        method: "POST",
        body: json<Wire.ReorderSeriesInput>({
          ordered_post_ids: orderedPostIds,
          expected_series_version: expectedSeriesVersion,
        }),
      },
    ),
};
