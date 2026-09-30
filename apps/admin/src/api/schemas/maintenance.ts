import { z } from "zod";
import type * as Wire from "../generated";
import { responseObject as object } from "../contract";
import { count, nullable, string } from "./primitives";

export const htmlRebuildCounts = object<Wire.HtmlRebuildCounts>()({
  posts: count, pages: count, comments: count,
});
export const htmlRebuildFailure = object<Wire.HtmlRebuildFailure>()({
  kind: z.enum(["post", "page", "comment"]).nullable(),
  id: nullable,
  message: string,
});
export const htmlRebuildReport = object<Wire.HtmlRebuildReport>()({
  rebuilt: htmlRebuildCounts,
  skipped: htmlRebuildCounts,
  pending: htmlRebuildCounts.nullable(),
  batches: count,
  has_more: z.boolean(),
  dry_run: z.boolean(),
  failure: htmlRebuildFailure.nullable(),
});
export const htmlRebuildJob = object<Wire.HtmlRebuildJob>()({
  id: string,
  status: z.enum(["running", "completed", "failed", "interrupted"]),
  report: htmlRebuildReport,
});
export const htmlRebuildView = object<Wire.HtmlRebuildView>()({
  pending: htmlRebuildCounts.nullable(),
  job: htmlRebuildJob.nullable(),
  available: z.boolean(),
});
