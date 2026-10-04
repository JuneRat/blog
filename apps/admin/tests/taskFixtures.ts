import type { TaskRun, TaskSchedule, TaskView } from "../src/api/generated";
import { counts } from "./maintenanceFixtures";

export function task(overrides: Partial<TaskRun> = {}): TaskRun {
  return { id: "0533b5bd-5ffc-4204-a009-282947495b99", kind: "html_rebuild", status: "queued", trigger: "manual",
    run_at: "2026-10-02T00:00:00Z", created_at: "2026-10-01T00:00:00Z", started_at: null, finished_at: null, retry_of: null,
    report: { html: null, retention: null, publication: null, error: null }, can_cancel: true, can_retry: false, ...overrides };
}
export function schedule(overrides: Partial<TaskSchedule> = {}): TaskSchedule {
  return { kind: "retention", enabled: false, interval_seconds: 86_400, next_run_at: null, version: 0, ...overrides };
}
export function taskView(overrides: Partial<TaskView> = {}): TaskView {
  return { available: true, retention_available: true, pending_html: counts(5, 2, 3), latest: [],
    schedules: [schedule()], runs: { items: [], next_cursor: null }, ...overrides };
}
