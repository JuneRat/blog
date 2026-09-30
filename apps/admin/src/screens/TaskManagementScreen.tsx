import { Alert, Button, Space, Tabs, Typography } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { tasksApi } from "../api/tasks";
import { ApiError } from "../api/client";
import type { TaskKind, TaskRun, TaskScheduleBody, TaskView } from "../api/generated";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import { useEditorRequestGuard } from "../useEditorRequestGuard";
import { useUnsavedGuard } from "../unsaved";
import { HtmlRebuildPanel } from "./tasks/HtmlRebuildPanel";
import { RetentionTaskPanel } from "./tasks/RetentionTaskPanel";
import { TaskHistoryPanel } from "./tasks/TaskHistoryPanel";

export function TaskManagementScreen() {
  const { me } = useAuth();
  const allowed = me?.permissions.includes("settings.manage") ?? false;
  const [kind, setKind] = useState<TaskKind | undefined>();
  const [history, setHistory] = useState<(string | undefined)[]>([undefined]);
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [lastView, setLastView] = useState<TaskView | undefined>();
  const [planDirty, setPlanDirty] = useState(false);
  useUnsavedGuard(planDirty, "清理计划有未保存的修改，离开会丢失。");
  const beginRequest = useEditorRequestGuard("task-management");
  const client = useQueryClient();
  const cursor = history[history.length - 1];
  const progress = useQuery({
    queryKey: queryKeys.tasks(kind, cursor), queryFn: ({ signal }) => tasksApi.get({ kind, cursor, limit: 20 }, signal),
    enabled: allowed, retry: false, staleTime: 0, gcTime: 0, refetchOnWindowFocus: false, refetchOnReconnect: false,
    placeholderData: previous => previous,
    refetchInterval: query => query.state.error !== null ? false
      : query.state.data?.latest.some(run => run.status === "running") ? 2_000 : 10_000,
  });
  useEffect(() => { if (progress.data) setLastView(progress.data); }, [progress.data]);
  const view = progress.data ?? lastView;
  const html = view?.latest.find(run => run.kind === "html_rebuild") ?? null;
  const retention = view?.latest.find(run => run.kind === "retention") ?? null;
  const publication = view?.latest.find(run => run.kind === "publish_due") ?? null;
  useEffect(() => {
    if (html && html.status !== "running" && html.status !== "queued" && (html.report.html?.rebuilt.comments ?? 0) > 0) {
      void client.invalidateQueries({ queryKey: queryKeys.commentsAll() });
    }
  }, [client, html?.id, html?.status, html?.report.html?.rebuilt.comments]);
  const available = allowed && view?.available === true && progress.error === null;
  const error = actionError ?? (progress.error ? progress.error instanceof ApiError ? permissionMessageOf(progress.error) : "无法连接服务器，请刷新任务状态后重试。" : null);

  async function operation<T>(action: () => Promise<T>, apply: (result: T) => void, success: string): Promise<T | null> {
    if (busyRef.current || !available) return null;
    const isCurrent = beginRequest();
    busyRef.current = true; setBusy(true); setActionError(null); setNotice(null);
    try {
      await client.cancelQueries({ queryKey: queryKeys.tasksAll() });
      const result = await action();
      apply(result);
      void client.invalidateQueries({ queryKey: queryKeys.tasksAll() });
      if (isCurrent()) setNotice(success);
      return result;
    } catch (cause) {
      if (isCurrent()) setActionError(cause instanceof ApiError ? permissionMessageOf(cause) : "无法连接服务器，请刷新任务状态后重试。");
      return null;
    } finally {
      busyRef.current = false;
      if (isCurrent()) setBusy(false);
    }
  }
  function applyRun(run: TaskRun) {
    client.setQueriesData<TaskView>({ queryKey: queryKeys.tasksAll() }, current => current ? ({ ...current,
      latest: [run, ...current.latest.filter(previous => previous.kind !== run.kind)],
      pending_html: run.kind === "html_rebuild" ? run.status === "running" ? null : run.report.html ? run.report.html.pending : current.pending_html : current.pending_html,
      runs: { ...current.runs, items: current.runs.items.map(previous => previous.id === run.id ? run : previous) },
    }) : current);
  }
  async function start(taskKind: TaskKind, at: string | null = null) {
    if (taskKind === "retention" && !view?.retention_available) return;
    await operation(() => tasksApi.start({ kind: taskKind, run_at: at }), applyRun, at ? "任务计划已创建。" : "任务已提交，离开页面后仍会继续执行。");
  }
  async function retry(run: TaskRun) {
    if (!run.can_retry || run.kind === "retention" && !view?.retention_available) return;
    await operation(() => tasksApi.retry(run.id), applyRun, "已创建新的重试任务，原记录保留。");
  }
  async function cancel(run: TaskRun) {
    if (!run.can_cancel) return;
    await operation(() => tasksApi.cancel(run.id), applyRun, "任务计划已取消。");
  }
  async function save(body: TaskScheduleBody) {
    if (body.enabled && !view?.retention_available) return null;
    return operation(() => tasksApi.saveRetentionSchedule(body), schedule => {
      client.setQueriesData<TaskView>({ queryKey: queryKeys.tasksAll() }, current => current ? ({ ...current, schedules: [schedule, ...current.schedules.filter(previous => previous.kind !== "retention")] }) : current);
    }, body.enabled ? "清理计划已保存。" : "清理计划已停止。");
  }
  if (!allowed) return <><Typography.Title level={3}>任务管理</Typography.Title><Alert type="warning" showIcon title="当前账号没有管理任务的权限。" /></>;
  return <>
    <Typography.Title level={3}>任务管理</Typography.Title>
    <Space style={{ marginBottom: 16 }}><Typography.Text type="secondary">任务和计划由服务端保存，刷新或重新登录后可继续查看。</Typography.Text>
      <Button disabled={busy || progress.isFetching} onClick={() => { setActionError(null); void progress.refetch(); }}>刷新任务状态</Button></Space>
    {error && <Alert type="error" showIcon title={error} description={progress.error ? "自动更新已暂停。请刷新任务状态确认执行结果，已提交的任务仍在服务端执行。" : "请刷新任务状态确认提交结果，已提交的任务仍在服务端执行。"} style={{ marginBottom: 16 }} />}
    {notice && <Alert type="success" showIcon title={notice} style={{ marginBottom: 16 }} />}
    {view?.available === false && <Alert type="warning" showIcon title="当前环境暂不能创建、重试、取消任务或修改计划。" style={{ marginBottom: 16 }} />}
    {!view && progress.isFetching && <Typography.Paragraph role="status">正在读取任务…</Typography.Paragraph>}
    {view && <Tabs items={[
      { key: "html", label: "内容重建", children: <HtmlRebuildPanel available={available} pending={view.pending_html} run={html} busy={busy}
        onStart={at => start("html_rebuild", at)} onRetry={retry} onCancel={cancel} /> },
      { key: "retention", label: "清理计划", forceRender: true, children: <RetentionTaskPanel available={available} retentionAvailable={view.retention_available}
        schedule={view.schedules.find(schedule => schedule.kind === "retention") ?? null} run={retention} busy={busy} onDirtyChange={setPlanDirty}
        onStart={() => start("retention")} onSave={save} /> },
      { key: "history", label: "任务记录", children: <TaskHistoryPanel page={progress.error ? { items: [], next_cursor: null } : view.runs} kind={kind} latestPublication={publication} readable={!progress.isError} busy={busy || progress.isFetching} available={available} retentionAvailable={view.retention_available} pageNumber={history.length}
        onKindChange={next => { setKind(next); setHistory([undefined]); }} onPrevious={() => setHistory(current => current.slice(0, -1))}
        onNext={() => { if (view.runs.next_cursor) setHistory(current => [...current, view.runs.next_cursor!]); }} onRetry={retry} onCancel={cancel} /> },
    ]} />}
  </>;
}
