import { Alert, Button, Input, Radio, Space, Spin, Table, Tag, Typography } from "antd";
import { useState } from "react";
import type { HtmlRebuildCounts } from "../../api/generated";
import type { TaskRun } from "../../api/generated";
import { dateTimeInput, formatDateTime, inputToInstant, invalidLocalTime } from "../../timeZone";
import { useTimeZone } from "../../timeZoneContext";
import { activeTask, statusLabels } from "./shared";

const rows = [{ key: "posts", label: "文章" }, { key: "pages", label: "独立页面" }, { key: "comments", label: "评论" }] as const;
const contentLabels = { post: "文章", page: "独立页面", comment: "评论" };

export function HtmlRebuildPanel({ available, pending, run, busy, onStart, onRetry, onCancel }: {
  available: boolean; pending: HtmlRebuildCounts | null; run: TaskRun | null; busy: boolean;
  onStart: (at: string | null) => Promise<void>; onRetry: (run: TaskRun) => Promise<void>; onCancel: (run: TaskRun) => Promise<void>;
}) {
  const timeZone = useTimeZone();
  const [mode, setMode] = useState<"now" | "once">("now");
  const [at, setAt] = useState("");
  const progress = run?.report.html;
  const active = run !== null && activeTask(run.status);
  const instant = inputToInstant(at, timeZone);
  const timestamp = instant ? Date.parse(instant) : NaN;
  const future = Number.isFinite(timestamp) && timestamp > Date.now() && timestamp <= Date.now() + 365 * 86_400_000;
  const total = pending === null ? null : pending.posts + pending.pages + pending.comments;
  const retryNow = mode === "now" && run?.can_retry === true;
  const label = active ? run.status === "queued" ? "等待执行" : "正在重建" : mode === "once" ? "创建重建计划" : retryNow ? "重新执行"
    : progress?.has_more && run?.status === "completed" ? "继续执行" : "开始重建";

  function changeMode(value: "now" | "once") {
    setMode(value);
    if (value === "once" && !at) setAt(dateTimeInput(new Date(Date.now() + 3_600_000).toISOString(), timeZone));
  }

  return <section aria-label="内容重建" style={{ maxWidth: 880 }}>
    <Typography.Title level={4}>重建内容 HTML</Typography.Title>
    <Typography.Paragraph type="secondary">更新文章、独立页面和评论的展示内容，不修改原文。每轮处理有上限，剩余内容可继续执行。已提交的任务由服务端持久保存，离开或刷新页面不会中止。</Typography.Paragraph>
    {run && <Space style={{ marginBottom: 16 }}><Tag color={active ? "processing" : run.status === "completed" ? "success" : "warning"}>{statusLabels[run.status]}</Tag>
      <Typography.Text type="secondary">计划执行：{formatDateTime(run.run_at, timeZone)}</Typography.Text></Space>}
    {run?.status === "queued" && <Alert type="info" showIcon title="任务正在等待执行。重启后会继续调度；可先取消，再重新设置计划。" style={{ marginBottom: 16 }} />}
    {run?.status === "running" && <div role="status" aria-live="polite" style={{ marginBottom: 16 }}><Spin size="small" /> 正在服务端执行，离开或刷新此页不会中止。</div>}
    {run?.status === "interrupted" && <Alert type="warning" showIcon title="上一次任务已中断。已完成的结果已保留；重新执行会创建新的任务记录。" style={{ marginBottom: 16 }} />}
    {run?.status === "failed" && <Alert type="error" showIcon title="内容重建未全部完成" description={<>
      {progress?.failure && <div>失败内容：{progress.failure.kind === null ? "未知类型" : contentLabels[progress.failure.kind]}，编号：{progress.failure.id ?? "未提供"}。</div>}
      已完成的结果已保留，可重新执行新任务。
    </>} style={{ marginBottom: 16 }} />}
    {run?.status === "completed" && <Alert type={progress?.has_more ? "info" : "success"} showIcon title={progress?.has_more ? "本轮已完成，仍有待重建内容，可继续执行。" : "内容重建完成。"} style={{ marginBottom: 16 }} />}
    <Table pagination={false} size="small" style={{ marginBottom: 16 }}
      dataSource={rows.map(row => ({ ...row, pending: pending?.[row.key] ?? "—", rebuilt: progress?.rebuilt[row.key] ?? "—", skipped: progress?.skipped[row.key] ?? "—" }))}
      columns={[{ title: "内容类型", dataIndex: "label" }, { title: "待重建", dataIndex: "pending" }, { title: "本轮已重建", dataIndex: "rebuilt" }, { title: "本轮跳过", dataIndex: "skipped" }]} />
    <Space orientation="vertical" size={12}>
      <Radio.Group aria-label="重建执行方式" value={mode} disabled={busy || active || !available} onChange={event => changeMode(event.target.value as "now" | "once")}
        options={[{ value: "now", label: "立即执行" }, { value: "once", label: "一次性计划" }]} />
      {mode === "once" && <>
        <Input aria-label={`重建执行时间（${timeZone}）`} type="datetime-local" value={at} disabled={busy || active || !available} onChange={event => setAt(event.target.value)} style={{ width: 260 }} />
        <Typography.Text type="secondary">站点时区：{timeZone}。可计划在未来一年内执行。</Typography.Text>
        {at && !instant && <Typography.Text type="danger" role="alert">{invalidLocalTime}</Typography.Text>}
        {instant && !future && <Typography.Text type="danger" role="alert">请选择晚于现在、且在未来一年内的时间。</Typography.Text>}
      </>}
      <Space>
        <Button type="primary" aria-label={label} loading={busy} disabled={!available || busy || active || (!retryNow && (total === null || total === 0 || mode === "once" && !future))}
          onClick={() => { if (retryNow && run) void onRetry(run); else void onStart(mode === "once" ? instant : null); }}>{label}</Button>
        {run?.can_cancel && <Button disabled={!available || busy} onClick={() => { void onCancel(run); }}>取消重建计划</Button>}
      </Space>
    </Space>
    {total === 0 && !active && <Typography.Paragraph type="secondary" style={{ marginTop: 12 }}>没有待重建内容。</Typography.Paragraph>}
  </section>;
}
