import type * as Wire from "./generated";
import * as s from "./schemas";
import { json, request, requestEmpty } from "./client";
import type { CommentItem, CommentPolicy } from "./responseTypes";

export const commentsApi = {
  list: (page: number, status: string, post?: string) => {
    const params = new URLSearchParams({ page: String(page) });
    if (status) params.set("status", status);
    if (post) params.set("post_id", post);
    return request(s.commentPage, `/api/admin/v1/comments?${params}`);
  },
  moderate: (item: CommentItem, status: string) =>
    requestEmpty(`/api/admin/v1/comments/${item.id}`, {
      method: "POST",
      body: json<Wire.ModerateCommentInput>({ version: item.version, status }),
    }),
  reply: (item: CommentItem, body: string) =>
    request(
      s.messageResult,
      `/api/v1/posts/${encodeURIComponent(item.post_slug)}/comments`,
      {
        method: "POST",
        body: json<Wire.SubmitCommentBody>({ body, parent_id: item.id }),
      },
    ),
  preview: (body: string) =>
    request(s.previewResult, "/api/v1/comments/preview", {
      method: "POST",
      body: json<Wire.CommentPreviewInput>({ body }),
    }),
  policy: (post?: string) => request(s.commentPolicy, commentPolicyPath(post)),
  savePolicy: (policy: CommentPolicy, post?: string) =>
    request(s.commentPolicy, commentPolicyPath(post), {
      method: "PUT",
      body: JSON.stringify(policy),
    }),
};

const commentPolicyPath = (post?: string) =>
  post
    ? `/api/admin/v1/posts/${encodeURIComponent(post)}/comment-settings`
    : "/api/admin/v1/comment-settings";
