import type * as Wire from "./generated";
import * as s from "./schemas";
import { json, request, requestEmpty } from "./client";
import type {
  ContentListFilter,
  ContentPage,
  PageDetail,
  PageSummary,
  PageTrash,
} from "../types";
import type { CreatePageInput, EditPageInput } from "./generated";
import { contentQuery } from "./contentQuery";

export const pagesApi = {
  listPages: (
    filter: Partial<ContentListFilter> = {},
  ): Promise<ContentPage<PageSummary>> =>
    request(s.pagePage, `/api/admin/v1/pages${contentQuery(filter)}`),

  getPage: (id: string): Promise<PageDetail> =>
    request(s.pageDetail, `/api/admin/v1/pages/${encodeURIComponent(id)}`),

  createPage: (input: CreatePageInput): Promise<PageDetail> =>
    request(s.pageDetail, "/api/admin/v1/pages", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  updatePage: (id: string, input: EditPageInput): Promise<PageDetail> =>
    request(s.pageDetail, `/api/admin/v1/pages/${encodeURIComponent(id)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  schedulePage: (
    id: string,
    publishedAt: string,
    expectedVersion?: number,
  ): Promise<PageDetail> =>
    request(
      s.pageDetail,
      `/api/admin/v1/pages/${encodeURIComponent(id)}/schedule`,
      {
        method: "POST",
        body: json<Wire.ScheduleInput>({
          published_at: publishedAt,
          expected_version: expectedVersion,
        }),
      },
    ),

  archivePage: (id: string, expectedVersion?: number): Promise<PageDetail> =>
    request(
      s.pageDetail,
      `/api/admin/v1/pages/${encodeURIComponent(id)}/archive`,
      {
        method: "POST",
        body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
      },
    ),

  publishPage: (id: string, expectedVersion?: number): Promise<PageDetail> =>
    request(
      s.pageDetail,
      `/api/admin/v1/pages/${encodeURIComponent(id)}/publish`,
      {
        method: "POST",
        body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
      },
    ),

  unpublishPage: (id: string, expectedVersion?: number): Promise<PageDetail> =>
    request(
      s.pageDetail,
      `/api/admin/v1/pages/${encodeURIComponent(id)}/unpublish`,
      {
        method: "POST",
        body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
      },
    ),

  listPageTrash: (page = 1): Promise<PageTrash> =>
    request(s.pagePage, `/api/admin/v1/page-trash?page=${page}`),

  trashPage: (id: string, expectedVersion: number): Promise<PageDetail> =>
    request(
      s.pageDetail,
      `/api/admin/v1/pages/${encodeURIComponent(id)}/trash`,
      {
        method: "POST",
        body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
      },
    ),

  restorePage: (id: string, expectedVersion: number): Promise<PageDetail> =>
    request(
      s.pageDetail,
      `/api/admin/v1/pages/${encodeURIComponent(id)}/restore`,
      {
        method: "POST",
        body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
      },
    ),

  purgePage: (id: string, expectedVersion: number): Promise<void> =>
    requestEmpty(`/api/admin/v1/pages/${encodeURIComponent(id)}/purge`, {
      method: "POST",
      body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
    }),
};
