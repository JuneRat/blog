import { formatDateTime, inputToInstant, invalidLocalTime } from "../timeZone";
import { useTimeZone } from "../timeZoneContext";
import { useState } from "react";
import { Alert, Button, Checkbox, Descriptions, Flex, Form, Input, Modal, Select, Table, Typography } from "antd";
import type { TableProps } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { auditApi } from "../api/audit";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import type { AuditFilter, AuditRecord } from "../types";

const targetNames: Record<string, string> = {
  post: "文章", page: "页面", comment: "评论", media: "媒体", user: "用户",
  role: "角色", system: "系统", settings: "设置", tag: "标签", category: "分类", series: "系列",
};

function actorName(item: AuditRecord): string {
  return item.actor_display ?? (item.actor_id ? "账号已不存在" : "无关联账号");
}

export function AuditLogScreen() {
  const timeZone = useTimeZone();
  const when = (value: string) => formatDateTime(value, timeZone);
  const { me } = useAuth();
  const allowed = me?.permissions.includes("audit.read") ?? false;
  const [form] = Form.useForm<AuditFilter>();
  const withoutActor = Form.useWatch("without_actor", form) === true;
  const [filter, setFilter] = useState<AuditFilter>({});
  // 保存实际边界，不依赖总数或可随保留期清理而移动的偏移。
  const [history, setHistory] = useState<(string | undefined)[]>([undefined]);
  const cursor = history[history.length - 1];
  const [detail, setDetail] = useState<AuditRecord | null>(null);
  const [inputError, setInputError] = useState<string | null>(null);
  const client = useQueryClient();
  const logs = useQuery({
    queryKey: queryKeys.auditLogs(filter, cursor),
    queryFn: () => auditApi.list(filter, cursor),
    enabled: allowed,
    // 敏感记录仅存在当前页面内存；返回页面时重新核验权限和保留期。
    gcTime: 0,
    staleTime: 0,
  });

  function apply(values: AuditFilter): void {
    const next: AuditFilter = {};
    for (const key of ["action", "actor_id", "target_type", "target_id"] as const) {
      const value = values[key]?.trim();
      if (value) next[key] = value;
    }
    if (values.without_actor) { next.without_actor = true; delete next.actor_id; }
    for (const key of ["from", "until"] as const) {
      if (values[key]) {
        const instant = inputToInstant(values[key], timeZone);
        if (!instant) { setInputError(invalidLocalTime); return; }
        next[key] = instant;
      }
    }
    if (next.from && next.until && next.from >= next.until) {
      setInputError("结束时间必须晚于开始时间。"); return;
    }
    setInputError(null);
    setFilter(next);
    setHistory([undefined]);
    setDetail(null);
    void client.invalidateQueries({ queryKey: ["audit-logs"] });
  }

  const columns: TableProps<AuditRecord>["columns"] = [
    { title: "时间", dataIndex: "created_at", width: 180, render: when },
    { title: "动作", dataIndex: "action", width: 195, render: (value: string) => <Typography.Text code>{value}</Typography.Text> },
    { title: "操作者", width: 155, render: (_, item) => <span title={item.actor_id ?? "访客、系统或 CLI"}>{actorName(item)}</span> },
    { title: "目标", width: 240, render: (_, item) => <><div>{targetNames[item.target_type] ?? item.target_type}</div><Typography.Text type="secondary" style={{ overflowWrap: "anywhere" }}>{item.target_id}</Typography.Text></> },
    { title: "来源 IP", dataIndex: "ip_address", width: 160, render: (value: string | null) => <span style={{ overflowWrap: "anywhere" }}>{value ?? "未记录"}</span> },
    { title: "摘要", width: 80, render: (_, item) => <Button type="link" onClick={() => setDetail(item)} aria-label={`查看 ${item.action} 详情`}>详情</Button> },
  ];

  if (!allowed) return <><Typography.Title level={3}>审计日志</Typography.Title><Alert type="warning" showIcon title="当前账号没有查看审计日志的权限。" /></>;
  return <>
    <Typography.Title level={3}>审计日志</Typography.Title>
    <Typography.Paragraph type="secondary">查看已成功提交的变更。记录按时间从新到旧排列，超过保留期的记录会定期清理。显示与筛选时区：{timeZone}。</Typography.Paragraph>
    <Form form={form} layout="vertical" onFinish={apply}>
      <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(210px, 1fr))", gap: "0 16px" }}>
        <Form.Item name="action" label="动作"><Input placeholder="例如 post.update" maxLength={128} allowClear /></Form.Item>
        <Form.Item name="target_type" label="目标类型"><Select allowClear placeholder="全部类型" options={Object.entries(targetNames).map(([value, label]) => ({ value, label }))} /></Form.Item>
        <Form.Item name="target_id" label="目标 ID"><Input maxLength={256} allowClear /></Form.Item>
        <Form.Item name="actor_id" label="操作者 ID"><Input placeholder="账号 UUID" disabled={withoutActor} allowClear /></Form.Item>
        <Form.Item name="from" label="开始时间（含）"><Input type="datetime-local" /></Form.Item>
        <Form.Item name="until" label="结束时间（不含）"><Input type="datetime-local" /></Form.Item>
      </div>
      <Flex gap={12} align="center" wrap style={{ marginBottom: 16 }}>
        <Form.Item name="without_actor" valuePropName="checked" noStyle><Checkbox>仅无关联账号（访客、系统或 CLI）</Checkbox></Form.Item>
        <Button type="primary" htmlType="submit" loading={logs.isFetching}>筛选</Button>
        <Button onClick={() => { form.resetFields(); apply({}); }}>重置</Button>
        <Button disabled={logs.isFetching} onClick={() => { setHistory([undefined]); setDetail(null); void client.invalidateQueries({ queryKey: ["audit-logs"] }); }}>刷新</Button>
      </Flex>
    </Form>
    {(inputError || logs.error) && <Alert type="error" showIcon title={inputError ?? permissionMessageOf(logs.error)} style={{ marginBottom: 16 }} />}
    <Table<AuditRecord> rowKey="id" size="middle" loading={logs.isFetching} dataSource={logs.error ? [] : logs.data?.items ?? []} columns={columns} pagination={false} scroll={{ x: 1010 }} locale={{ emptyText: logs.error ? "审计日志加载失败。" : "没有符合条件的记录。" }} />
    <Flex gap={12} justify="flex-end" align="center" style={{ marginTop: 16 }}>
      <Typography.Text type="secondary">第 {history.length} 页</Typography.Text>
      <Button disabled={history.length === 1 || logs.isFetching} onClick={() => setHistory(previous => previous.slice(0, -1))}>上一页</Button>
      <Button disabled={!logs.data?.next_cursor || logs.isFetching || logs.isError} onClick={() => { const next = logs.data?.next_cursor; if (next) setHistory(previous => [...previous, next]); }}>下一页</Button>
    </Flex>
    <Modal title="审计详情" open={detail !== null && !logs.isError} onCancel={() => setDetail(null)} footer={<Button onClick={() => setDetail(null)}>关闭</Button>} destroyOnHidden>
      {detail && <Descriptions column={1} size="small" styles={{ content: { overflowWrap: "anywhere", whiteSpace: "pre-wrap" } }} items={[
        { key: "id", label: "记录 ID", children: detail.id },
        { key: "at", label: "时间", children: when(detail.created_at) },
        { key: "actor", label: "操作者", children: `${actorName(detail)}${detail.actor_id ? ` · ${detail.actor_id}` : "（访客、系统或 CLI）"}` },
        { key: "ip", label: "来源 IP", children: detail.ip_address ?? "未记录" },
        { key: "action", label: "动作", children: detail.action },
        { key: "target", label: "目标", children: `${detail.target_type} · ${detail.target_id}` },
        ...detail.summary.map(field => ({ key: `summary:${field.key}`, label: field.key, children: field.value })),
      ]} />}
    </Modal>
  </>;
}
