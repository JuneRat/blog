import { Alert, Button, Space, Spin, Table, Tag, Typography } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { maintenanceApi } from "../../api/maintenance";
import { ApiError } from "../../api/client";
import type { HtmlRebuildView } from "../../api/generated";
import { permissionMessageOf } from "../../apiError";
import { queryKeys } from "../../queryClient";

const statusLabels = {
  running: "正在重建", completed: "本轮已完成", failed: "执行失败", interrupted: "已中断",
};
const contentLabels = { post: "文章", page: "独立页面", comment: "评论" };
const rows = [
  { key: "posts", label: "文章" },
  { key: "pages", label: "独立页面" },
  { key: "comments", label: "评论" },
] as const;

function requestError(cause: unknown): string {
  return cause instanceof ApiError ? permissionMessageOf(cause) : "无法连接服务器，请检查网络后重试。";
}

export function HtmlRebuildPanel({ active }: { active: boolean }) {
  const client = useQueryClient();
  const mounted = useRef(true);
  const starting = useRef(false);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  const progress = useQuery({
    queryKey: queryKeys.htmlRebuild(),
    queryFn: ({ signal }) => maintenanceApi.get(signal),
    enabled: active,
    staleTime: 0,
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
    refetchInterval: query => query.state.error === null && query.state.data?.available
      && query.state.data.job?.status === "running" ? 2_000 : false,
  });
  const view = progress.data;
  const job = view?.job;
  const running = job?.status === "running";
  useEffect(() => {
    if (job && job.status !== "running" && job.report.rebuilt.comments > 0) {
      // Admin comment queries include rendered HTML; partial success changes it too.
      void client.invalidateQueries({ queryKey: queryKeys.commentsAll() });
    }
  }, [client, job?.id, job?.status, job?.report.rebuilt.comments]);
  const pending = view?.pending;
  const total = pending == null ? null : pending.posts + pending.pages + pending.comments;
  const error = actionError ?? (progress.error === null ? null : requestError(progress.error));

  async function start() {
    if (starting.current || !view?.available || running || total === null || total === 0 || progress.error) return;
    starting.current = true;
    setBusy(true);
    setActionError(null);
    try {
      // A pending GET must not overwrite the newer POST result with an idle view.
      await client.cancelQueries({ queryKey: queryKeys.htmlRebuild() });
      const started = await maintenanceApi.start();
      client.setQueryData<HtmlRebuildView>(queryKeys.htmlRebuild(), current => ({
        available: current?.available ?? true,
        pending: started.status === "running" ? null : started.report.pending,
        job: started,
      }));
    } catch (cause) {
      if (mounted.current) setActionError(requestError(cause));
    } finally {
      starting.current = false;
      if (mounted.current) setBusy(false);
    }
  }

  function refresh() {
    setActionError(null);
    void progress.refetch();
  }

  const buttonLabel = job?.status === "failed" ? "再次执行"
    : job?.status === "interrupted" || job?.status === "completed" && job.report.has_more ? "继续执行"
    : "开始重建";

  return <section aria-label="内容维护" style={{ maxWidth: 880, paddingTop: 8 }}>
    <Typography.Title level={4} style={{ marginTop: 0 }}>重建内容 HTML</Typography.Title>
    <Typography.Paragraph type="secondary">
      根据当前渲染规则更新文章、独立页面和评论的展示内容，不修改原文。每轮处理有上限，剩余内容可继续执行。
    </Typography.Paragraph>
    {error !== null && <Alert type="error" showIcon title={error}
      description="进度更新已暂停，请刷新进度确认任务状态。已启动的任务仍会在服务端继续执行。"
      style={{ marginBottom: 16 }} />}
    {view === undefined && progress.isFetching && <div role="status"><Spin size="small" /> 正在读取待重建内容…</div>}
    {view?.available === false && <Alert type="warning" showIcon title="恢复隔离期间无法执行内容维护"
      description="请先完成恢复检查并解除隔离，再刷新进度。" style={{ marginBottom: 16 }} />}
    {job && <Space style={{ marginBottom: 16 }}>
      <Tag color={running ? "processing" : job.status === "completed" ? "success" : "warning"}>{statusLabels[job.status]}</Tag>
      <Typography.Text type="secondary">已处理 {job.report.batches} 批</Typography.Text>
    </Space>}
    {running && <div role="status" aria-live="polite" style={{ marginBottom: 16 }}>
      <Spin size="small" /> 任务正在服务端执行。离开或刷新此页不会中止，返回后可继续查看进度。
    </div>}
    {job?.status === "completed" && <Alert showIcon type={job.report.has_more ? "info" : "success"}
      title={job.report.has_more ? "本轮已完成，仍有待重建内容，可继续执行。" : "内容重建完成。"} style={{ marginBottom: 16 }} />}
    {job?.status === "interrupted" && <Alert type="warning" showIcon
      title="上一次重建已中断，已完成的结果已保留，可继续执行剩余内容。" style={{ marginBottom: 16 }} />}
    {job?.status === "failed" && <Alert type="error" showIcon title="内容重建未全部完成"
      description={<>
        {job.report.failure && <div>失败内容：{job.report.failure.kind === null ? "未知类型" : contentLabels[job.report.failure.kind]}，编号：{job.report.failure.id ?? "未提供"}。</div>}
        已完成的结果已保留，请再次执行处理剩余内容。
      </>} style={{ marginBottom: 16 }} />}
    {view && <Table pagination={false} size="small" style={{ marginBottom: 16 }}
      dataSource={rows.map(row => ({ ...row, pending: view.pending?.[row.key] ?? "—",
        rebuilt: job?.report.rebuilt[row.key] ?? "—", skipped: job?.report.skipped[row.key] ?? "—" }))}
      columns={[
        { title: "内容类型", dataIndex: "label" },
        { title: "待重建", dataIndex: "pending" },
        { title: "本轮已重建", dataIndex: "rebuilt" },
        { title: "本轮跳过", dataIndex: "skipped" },
      ]} />}
    <Space>
      <Button type="primary" aria-label={running ? "正在重建" : buttonLabel} loading={busy}
        disabled={!active || busy || !view?.available || running || total === null || total === 0 || progress.error !== null} onClick={() => { void start(); }}>
        {running ? "正在重建" : buttonLabel}
      </Button>
      <Button onClick={refresh} disabled={!active || busy || progress.isFetching}>刷新进度</Button>
    </Space>
    {view?.available && total === 0 && !running && <Typography.Paragraph type="secondary" style={{ marginTop: 12 }}>没有待重建内容。</Typography.Paragraph>}
  </section>;
}
