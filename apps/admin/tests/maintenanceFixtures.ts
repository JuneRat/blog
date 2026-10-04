import type { HtmlRebuildCounts, HtmlRebuildJob, HtmlRebuildReport, HtmlRebuildView } from "../src/api/generated";

export const counts = (posts = 0, pages = 0, comments = 0): HtmlRebuildCounts => ({ posts, pages, comments });
export const report = (overrides: Partial<HtmlRebuildReport> = {}): HtmlRebuildReport => ({
  rebuilt: counts(), skipped: counts(), pending: counts(5, 2, 3), batches: 0,
  has_more: true, dry_run: false, failure: null, ...overrides,
});
export const job = (status: HtmlRebuildJob["status"] = "running", progress: Partial<HtmlRebuildReport> = {}): HtmlRebuildJob => ({
  id: "b95d931d-1e55-41d6-924f-6b4356f2d575", status, report: report({ pending: status === "running" ? null : counts(5, 2, 3), ...progress }),
});
export const view = (overrides: Partial<HtmlRebuildView> = {}): HtmlRebuildView => ({
  available: true, pending: overrides.job?.status === "running" ? null : counts(5, 2, 3), job: null, ...overrides,
});
