import type { z } from "zod";
import type * as s from "./schemas";
export type CommentItem = z.output<typeof s.commentItem>;
export type CommentPage = z.output<typeof s.commentPage>;
export type CommentPolicy = z.output<typeof s.commentPolicy>;
