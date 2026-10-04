import type { TaskKind, TaskStatus, TaskTrigger } from "../../api/generated";
export const kindLabels: Record<TaskKind, string> = { html_rebuild: "HTML 重建", retention: "保留期清理", publish_due: "预约发布" };
export const statusLabels: Record<TaskStatus, string> = {
  queued: "等待执行", running: "正在执行", completed: "已完成", failed: "执行失败", interrupted: "已中断", cancelled: "已取消",
};
export const triggerLabels: Record<TaskTrigger, string> = { manual: "立即执行", once: "一次性计划", periodic: "周期任务", retry: "重新执行" };
export const activeTask = (status: TaskStatus) => status === "queued" || status === "running";
