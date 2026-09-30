import { z } from "zod";
import type * as Wire from "../generated";
import { responseObject as object } from "../contract";
import { string, nullable, count } from "./primitives";

export const commentItem = object<Wire.CommentItem>()({
  moderation_reason: nullable,
  id: string,
  post_id: string,
  post_slug: string,
  post_title: string,
  parent_id: nullable,
  root_id: nullable,
  parent_nickname: nullable,
  author_email: nullable,
  ip_address: nullable,
  content_html: string,
  nickname: string,
  body: string,
  is_author: z.boolean(),
  status: string,
  version: count,
  created_at: string,
});
export const commentPage = object<Wire.CommentPage>()({
  items: z.array(commentItem),
  total: count,
  enabled: z.boolean(),
});
export const commentPolicy = object<Wire.CommentPolicy>()({
  moderation: z.enum(['all', 'guests', 'first_comment', 'none']).nullish(),
  enabled: z.boolean(),
  version: count,
});
export const commentSubmissionResult = object<Wire.CommentSubmissionResult>()({
  message: string,
  status: z.enum(['pending', 'approved']),
});
