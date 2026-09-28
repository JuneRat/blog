import type * as Wire from "./generated";
import * as s from "./schemas";
import { json, request, requestEmpty } from "./client";
import type {
  ContentListFilter,
  ContentPage,
  PostDetail,
  PostSummary,
} from "../types";
import type { CreatePostInput, EditPostInput } from "./generated";
import { contentQuery } from "./contentQuery";

export const postsApi = {
  listPosts: (
    filter: Partial<ContentListFilter> & { author?: string } = {},
  ): Promise<ContentPage<PostSummary>> =>
    request(s.postPage, `/api/admin/v1/posts${contentQuery(filter)}`),

  listTrash: (
    page = 1,
    author?: string,
  ): Promise<{
    items: PostSummary[];
    total: number;
    page: number;
    per_page: number;
  }> =>
    request(
      s.postPage,
      `/api/admin/v1/post-trash?page=${page}${author ? `&author=${encodeURIComponent(author)}` : ""}`,
    ),

  trashPost: (id: string, expectedVersion: number): Promise<PostDetail> =>
    request(
      s.postDetail,
      `/api/admin/v1/posts/${encodeURIComponent(id)}/trash`,
      {
        method: "POST",
        body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
      },
    ),

  restorePost: (id: string, expectedVersion: number): Promise<PostDetail> =>
    request(
      s.postDetail,
      `/api/admin/v1/posts/${encodeURIComponent(id)}/restore`,
      {
        method: "POST",
        body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
      },
    ),

  purgePost: (id: string, expectedVersion: number): Promise<void> =>
    requestEmpty(`/api/admin/v1/posts/${encodeURIComponent(id)}/purge`, {
      method: "POST",
      body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
    }),

  getPost: (id: string): Promise<PostDetail> =>
    request(s.postDetail, `/api/admin/v1/posts/${encodeURIComponent(id)}`),

  createPost: (input: CreatePostInput): Promise<PostDetail> =>
    request(s.postDetail, "/api/admin/v1/posts", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  updatePost: (id: string, input: EditPostInput): Promise<PostDetail> =>
    request(s.postDetail, `/api/admin/v1/posts/${encodeURIComponent(id)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  schedulePost: (
    id: string,
    publishedAt: string,
    expectedVersion?: number,
  ): Promise<PostDetail> =>
    request(
      s.postDetail,
      `/api/admin/v1/posts/${encodeURIComponent(id)}/schedule`,
      {
        method: "POST",
        body: json<Wire.ScheduleInput>({
          published_at: publishedAt,
          expected_version: expectedVersion,
        }),
      },
    ),

  archivePost: (id: string, expectedVersion?: number): Promise<PostDetail> =>
    request(
      s.postDetail,
      `/api/admin/v1/posts/${encodeURIComponent(id)}/archive`,
      {
        method: "POST",
        body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
      },
    ),

  publishPost: (id: string, expectedVersion?: number): Promise<PostDetail> =>
    request(
      s.postDetail,
      `/api/admin/v1/posts/${encodeURIComponent(id)}/publish`,
      {
        method: "POST",
        body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
      },
    ),

  unpublishPost: (id: string, expectedVersion?: number): Promise<PostDetail> =>
    request(
      s.postDetail,
      `/api/admin/v1/posts/${encodeURIComponent(id)}/unpublish`,
      {
        method: "POST",
        body: json<Wire.VersionInput>({ expected_version: expectedVersion }),
      },
    ),
};
