import { Alert, Button, Checkbox, Input, InputNumber, Space, Typography } from "antd";
import { useEffect, useState } from "react";
import type { TaskRun, TaskSchedule, TaskScheduleBody } from "../../api/generated";
import { dateTimeInput, formatDateTime, inputToInstant, invalidLocalTime } from "../../timeZone";
import { useTimeZone } from "../../timeZoneContext";
import { activeTask, statusLabels } from "./shared";

interface Fields { enabled: boolean; hours: number | null; at: string }
export function RetentionTaskPanel({ schedule, run, available, retentionAvailable, busy, onStart, onSave, onDirtyChange }: {
  schedule: TaskSchedule | null; run: TaskRun | null; available: boolean; retentionAvailable: boolean; busy: boolean;
  onStart: () => Promise<void>; onSave: (body: TaskScheduleBody) => Promise<TaskSchedule | null>;
  onDirtyChange: (dirty: boolean) => void;
}) {
  const timeZone = useTimeZone();
  const [baseline, setBaseline] = useState<{ schedule: TaskSchedule; fields: Fields } | null>(null);
  const [fields, setFields] = useState<Fields>({ enabled: false, hours: 24, at: "" });
  const dirty = baseline !== null && (fields.enabled !== baseline.fields.enabled || fields.hours !== baseline.fields.hours || fields.at !== baseline.fields.at);
  useEffect(() => { onDirtyChange(dirty); }, [dirty, onDirtyChange]);
  useEffect(() => {
    if (schedule && !dirty) {
      const next = { enabled: schedule.enabled, hours: schedule.interval_seconds / 3_600, at: dateTimeInput(schedule.next_run_at, timeZone) };
      setBaseline({ schedule, fields: next }); setFields(next);
    }
  }, [schedule, timeZone, dirty]);
  const instant = fields.at ? inputToInstant(fields.at, timeZone) : null;
  const seconds = fields.hours === null ? null : Math.round(fields.hours * 3_600);
  const validHours = seconds !== null && Number.isSafeInteger(seconds) && seconds >= 3_600 && seconds <= 2_592_000;
  const effectiveInstant = baseline && fields.at === baseline.fields.at ? baseline.schedule.next_run_at : instant;
  const validAt = !fields.enabled || !fields.at || effectiveInstant !== null && Date.parse(effectiveInstant) > Date.now();

  async function save() {
    if (!baseline || !validHours || !validAt || !available || fields.enabled && !retentionAvailable || busy) return;
    const saved = await onSave({ enabled: fields.enabled, interval_seconds: seconds!,
      next_run_at: !fields.enabled ? null : effectiveInstant,
      version: baseline.schedule.version });
    if (saved) {
      const next = { enabled: saved.enabled, hours: saved.interval_seconds / 3_600, at: dateTimeInput(saved.next_run_at, timeZone) };
      setBaseline({ schedule: saved, fields: next }); setFields(next);
    }
  }
  function reload() {
    if (!schedule) return;
    const next = { enabled: schedule.enabled, hours: schedule.interval_seconds / 3_600, at: dateTimeInput(schedule.next_run_at, timeZone) };
    setBaseline({ schedule, fields: next }); setFields(next);
  }

  return <section aria-label="清理计划" style={{ maxWidth: 880 }}>
    <Typography.Title level={4}>保留期清理</Typography.Title>
    <Typography.Paragraph type="secondary">按站点设置中的保留期限清理评论 IP 与审计日志。周期按固定时间间隔执行，不保证每天在同一本地钟点运行。</Typography.Paragraph>
    {!retentionAvailable && <Alert type="warning" showIcon title="当前未配置可用的清理执行环境" description="暂不能执行或开启清理计划；已有计划仍可停止。" style={{ marginBottom: 16 }} />}
    {run && <Typography.Paragraph>最近任务：{statusLabels[run.status]}。{run.report.retention && <>已清理评论 IP {run.report.retention.comment_ips} 条，审计日志 {run.report.retention.audit_logs} 条。</>}</Typography.Paragraph>}
    {run?.status === "completed" && run.report.retention?.has_more && <Typography.Paragraph type="secondary">本轮已结束，仍有待清理数据，可立即清理或等待下一周期。</Typography.Paragraph>}
    <Button disabled={!available || !retentionAvailable || busy || run !== null && activeTask(run.status)} onClick={() => { void onStart(); }}>立即清理</Button>
    <Typography.Title level={5}>周期计划</Typography.Title>
    {schedule && <Typography.Paragraph type="secondary">当前计划：{schedule.enabled ? "已启用" : "已停止"}。{schedule.next_run_at && <>下次执行：{formatDateTime(schedule.next_run_at, timeZone)}。</>}</Typography.Paragraph>}
    <Space orientation="vertical" size={12}>
      <Checkbox checked={fields.enabled} disabled={!available || busy || !retentionAvailable && !fields.enabled} onChange={event => setFields(current => ({ ...current, enabled: event.target.checked }))}>启用周期清理</Checkbox>
      <Space><Typography.Text>间隔（小时）</Typography.Text><InputNumber aria-label="清理间隔（小时）" min={1} max={720} value={fields.hours} disabled={!available || !retentionAvailable || busy} onChange={hours => setFields(current => ({ ...current, hours }))} /></Space>
      <Input aria-label={`首次清理时间（${timeZone}）`} type="datetime-local" value={fields.at} disabled={!available || !retentionAvailable || busy} onChange={event => setFields(current => ({ ...current, at: event.target.value }))} style={{ width: 260 }} />
      <Typography.Text type="secondary">站点时区：{timeZone}。时间留空时，首次执行设为当前时刻加清理间隔。支持 1 至 720 小时。</Typography.Text>
      {fields.enabled && fields.at && !effectiveInstant && <Typography.Text type="danger" role="alert">{invalidLocalTime}</Typography.Text>}
      {fields.enabled && effectiveInstant && !validAt && <Typography.Text type="danger" role="alert">首次执行时间必须晚于现在。</Typography.Text>}
      <Space><Button type="primary" aria-label="保存清理计划" loading={busy} disabled={!baseline || !dirty || !available || busy || fields.enabled && !retentionAvailable || !validHours || !validAt} onClick={() => { void save(); }}>保存清理计划</Button>
        {dirty && <Button disabled={busy} onClick={reload}>重新加载计划并放弃修改</Button>}</Space>
    </Space>
  </section>;
}
