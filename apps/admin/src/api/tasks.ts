import type { TaskKind, TaskRun, TaskSchedule, TaskScheduleBody, TaskStartBody, TaskView } from "./generated";
import { json, request } from "./client";
import { taskRun, taskSchedule, taskView } from "./schemas/tasks";

export interface TaskFilter { kind?: TaskKind; cursor?: string; limit?: number }
const endpoint = "/api/admin/v1/tasks";
export const tasksApi = {
  get: (filter: TaskFilter = {}, signal?: AbortSignal): Promise<TaskView> => {
    const params = new URLSearchParams();
    if (filter.kind) params.set("kind", filter.kind);
    if (filter.cursor) params.set("cursor", filter.cursor);
    params.set("limit", String(filter.limit ?? 20));
    return request(taskView, `${endpoint}?${params}`, { signal });
  },
  start: (body: TaskStartBody): Promise<TaskRun> => request(taskRun, endpoint, { method: "POST", body: json<TaskStartBody>(body) }),
  retry: (id: string): Promise<TaskRun> => request(taskRun, `${endpoint}/${encodeURIComponent(id)}/retry`, { method: "POST" }),
  cancel: (id: string): Promise<TaskRun> => request(taskRun, `${endpoint}/${encodeURIComponent(id)}/cancel`, { method: "POST" }),
  saveRetentionSchedule: (body: TaskScheduleBody): Promise<TaskSchedule> => request(taskSchedule, `${endpoint}/retention-schedule`, { method: "PUT", body: json<TaskScheduleBody>(body) }),
};
