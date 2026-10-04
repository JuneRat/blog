import { z } from "zod";
import type * as Wire from "../generated";
import { responseObject as object } from "../contract";
import { count, nullable, string } from "./primitives";
import { htmlRebuildCounts, htmlRebuildReport } from "./maintenance";

export const taskKind = z.enum(["html_rebuild", "retention", "publish_due"]);
export const taskRetentionResult = object<Wire.TaskRetentionResult>()({
  comment_ips: count, audit_logs: count, batches: count, has_more: z.boolean(), dry_run: z.boolean(),
});
export const taskPublicationResult = object<Wire.TaskPublicationResult>()({
  published: count, batches: count, has_more: z.boolean(),
});
export const taskReport = object<Wire.TaskReport>()({
  html: htmlRebuildReport.nullable(), retention: taskRetentionResult.nullable(),
  publication: taskPublicationResult.nullable(), error: nullable,
});
export const taskRun = object<Wire.TaskRun>()({
  id: string, kind: taskKind, status: z.enum(["queued", "running", "completed", "failed", "interrupted", "cancelled"]),
  trigger: z.enum(["manual", "once", "periodic", "retry"]), run_at: string, created_at: string,
  started_at: nullable, finished_at: nullable, retry_of: nullable, report: taskReport,
  can_retry: z.boolean(), can_cancel: z.boolean(),
});
export const taskSchedule = object<Wire.TaskSchedule>()({
  kind: taskKind, enabled: z.boolean(), interval_seconds: count, next_run_at: nullable, version: count,
});
export const taskRunPage = object<Wire.TaskRunPage>()({ items: z.array(taskRun), next_cursor: nullable });
export const taskView = object<Wire.TaskView>()({
  available: z.boolean(), retention_available: z.boolean(), pending_html: htmlRebuildCounts.nullable(),
  latest: z.array(taskRun), schedules: z.array(taskSchedule), runs: taskRunPage,
});
