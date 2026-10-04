import { Alert, Button, Descriptions, Flex, Modal, Select, Space, Table, Tag, Typography } from "antd";
import { useEffect, useState } from "react";
import type { TaskKind, TaskRun, TaskRunPage } from "../../api/generated";
import { formatDateTime } from "../../timeZone";
import { useTimeZone } from "../../timeZoneContext";
import { kindLabels, statusLabels, triggerLabels } from "./shared";

export function TaskHistoryPanel({ page, kind, latestPublication, readable, busy, available, retentionAvailable, pageNumber, onKindChange, onPrevious, onNext, onRetry, onCancel }: {
  page: TaskRunPage; kind?: TaskKind; latestPublication: TaskRun | null; readable: boolean; busy: boolean; available: boolean; retentionAvailable: boolean; pageNumber: number;
  onKindChange: (kind: TaskKind | undefined) => void; onPrevious: () => void; onNext: () => void;
  onRetry: (run: TaskRun) => Promise<void>; onCancel: (run: TaskRun) => Promise<void>;
}) {
  const timeZone = useTimeZone();
  const [detail, setDetail] = useState<TaskRun | null>(null);
  useEffect(() => { if (!readable) setDetail(null); }, [readable]);
  const when = (value: string | null) => value ? formatDateTime(value, timeZone) : "—";
  const selected = readable && detail ? page.items.find(run => run.id === detail.id) ?? detail : null;
  return <section aria-label="任务记录">
    <Alert type="info" showIcon title="预约发布由系统每 30 秒自动检查，不需要手动创建计划。"
      description={latestPublication ? `最近状态：${statusLabels[latestPublication.status]}；本轮发布 ${latestPublication.report.publication?.published ?? 0} 条。${latestPublication.report.publication?.has_more ? "仍有到期内容，系统将继续处理。" : ""}` : "暂未生成预约发布记录。"} style={{ marginBottom: 16 }} />
    <Typography.Paragraph type="secondary">每类任务最多保留 500 条历史。排队任务重启后会继续调度；中断任务需重新执行，生成新的记录。</Typography.Paragraph>
    <Select aria-label="任务类型筛选" value={kind} allowClear placeholder="全部任务类型" style={{ width: 220, marginBottom: 16 }} onChange={onKindChange}
      options={Object.entries(kindLabels).map(([value, label]) => ({ value, label }))} />
    <Table<TaskRun> rowKey="id" pagination={false} dataSource={page.items} scroll={{ x: 1050 }} columns={[
      { title: "任务类型", render: (_, run) => kindLabels[run.kind] },
      { title: "执行方式", render: (_, run) => triggerLabels[run.trigger] },
      { title: "状态", render: (_, run) => <Tag>{statusLabels[run.status]}</Tag> },
      { title: "计划执行", dataIndex: "run_at", render: when },
      { title: "开始时间", dataIndex: "started_at", render: when },
      { title: "结束时间", dataIndex: "finished_at", render: when },
      { title: "操作", render: (_, run) => <Space><Button aria-label={`查看任务 ${run.id}`} onClick={() => setDetail(run)}>详情</Button>
        {run.can_retry && <Button disabled={!available || busy || run.kind === "retention" && !retentionAvailable} aria-label={`重新执行任务 ${run.id}`} onClick={() => { void onRetry(run); }}>重新执行</Button>}
        {run.can_cancel && <Button disabled={!available || busy} aria-label={`取消任务 ${run.id}`} onClick={() => { void onCancel(run); }}>取消</Button>}</Space> },
    ]} locale={{ emptyText: "没有符合条件的任务记录。" }} />
    <Flex gap={12} justify="flex-end" align="center" style={{ marginTop: 16 }}><Typography.Text>第 {pageNumber} 页</Typography.Text>
      <Button disabled={pageNumber === 1 || busy} onClick={onPrevious}>上一页</Button><Button disabled={!page.next_cursor || busy} onClick={onNext}>下一页</Button></Flex>
    {readable && <Modal title="任务详情" open={selected !== null} onCancel={() => setDetail(null)} footer={<Button onClick={() => setDetail(null)}>关闭</Button>} destroyOnHidden>
      {selected && <Descriptions column={1} size="small" items={[
        { key: "id", label: "任务编号", children: selected.id },
        { key: "status", label: "状态", children: statusLabels[selected.status] },
        { key: "retry", label: "原任务编号", children: selected.retry_of ?? "—" },
        ...(selected.report.html ? [{ key: "html", label: "HTML 重建", children: `文章 ${selected.report.html.rebuilt.posts}、页面 ${selected.report.html.rebuilt.pages}、评论 ${selected.report.html.rebuilt.comments}；跳过文章 ${selected.report.html.skipped.posts}、页面 ${selected.report.html.skipped.pages}、评论 ${selected.report.html.skipped.comments}；${selected.report.html.batches} 批` }] : []),
        ...(selected.report.html?.failure ? [{ key: "failure", label: "失败内容", children: `${selected.report.html.failure.kind === null ? "未知类型" : ({ post: "文章", page: "独立页面", comment: "评论" })[selected.report.html.failure.kind]} · ${selected.report.html.failure.id ?? "未提供编号"}` }] : []),
        ...(selected.report.retention ? [{ key: "retention", label: "清理结果", children: `评论 IP ${selected.report.retention.comment_ips}、审计日志 ${selected.report.retention.audit_logs}；${selected.report.retention.batches} 批` }] : []),
        ...(selected.report.publication ? [{ key: "publication", label: "发布结果", children: `发布 ${selected.report.publication.published} 条；${selected.report.publication.batches} 批` }] : []),
        ...(selected.status === "failed" || selected.status === "interrupted" ? [{ key: "error", label: "失败说明", children: selected.status === "interrupted" ? "任务已中断，已完成的变更已保留。请重新执行新任务。" : "任务执行未全部完成，已完成的变更已保留。请检查执行环境后重新执行。" }] : []),
      ]} />}
    </Modal>}
  </section>;
}
