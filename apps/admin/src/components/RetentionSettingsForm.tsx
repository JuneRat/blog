import { Alert, Button, Form, InputNumber, Space, Typography } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { ApiError } from "../api/client";
import { retentionApi } from "../api/settings";
import type { RetentionSettings } from "../api/generated";
import { permissionMessageOf } from "../apiError";
import { queryKeys } from "../queryClient";

export function RetentionSettingsForm({ onDirtyChange }: { onDirtyChange: (dirty: boolean) => void }) {
  const client = useQueryClient();
  const query = useQuery({ queryKey: queryKeys.retentionSettings(), queryFn: retentionApi.get });
  const [base, setBase] = useState<RetentionSettings | null>(null);
  const [ipDays, setIpDays] = useState<number | null>(180);
  const [auditDays, setAuditDays] = useState<number | null>(180);
  const [busy, setBusy] = useState(false);
  const [conflict, setConflict] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const dirty = base !== null && (ipDays !== base.comment_ip_days || auditDays !== base.audit_days);
  useEffect(() => { onDirtyChange(dirty); }, [dirty, onDirtyChange]);
  useEffect(() => {
    if (base === null && query.data) {
      setBase(query.data); setIpDays(query.data.comment_ip_days); setAuditDays(query.data.audit_days);
    }
  }, [base, query.data]);
  const apply = (value: RetentionSettings) => {
    setBase(value); setIpDays(value.comment_ip_days); setAuditDays(value.audit_days);
    client.setQueryData(queryKeys.retentionSettings(), value);
  };
  const save = async () => {
    if (!base || busy || conflict) return;
    if (![ipDays, auditDays].every(v => v !== null && Number.isInteger(v) && v >= 1 && v <= 36500)) {
      setError("保留期须为 1–36,500 的整数天数。"); return;
    }
    setBusy(true); setError(null); setNotice(null);
    try {
      apply(await retentionApi.save({ ...base, comment_ip_days: ipDays!, audit_days: auditDays! }));
      setNotice("保留期已保存，下次维护时生效。");
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        setConflict(true); setError("设置已在别处修改。你的输入已保留，请重新加载后再编辑。");
      } else setError(permissionMessageOf(e));
    } finally { setBusy(false); }
  };
  const reload = async () => {
    setBusy(true); setError(null); setNotice(null);
    try { apply(await retentionApi.get()); setConflict(false); }
    catch (e) { setError(permissionMessageOf(e)); }
    finally { setBusy(false); }
  };
  return <section aria-label="数据保留期">
    <Typography.Title level={3}>数据保留期</Typography.Title>
    <Typography.Paragraph type="secondary">
      默认保留 180 天，按创建时间计算。维护任务会清空过期评论的 IP，并永久删除过期审计记录。
    </Typography.Paragraph>
    {(error ?? (query.error ? permissionMessageOf(query.error) : null)) &&
      <Alert type="error" showIcon title={error ?? permissionMessageOf(query.error)} />}
    {notice && <Alert type="success" title={notice} />}
    {base && <Form layout="vertical" onFinish={() => void save()} style={{ maxWidth: 420 }}>
      <Form.Item label="评论 IP 保留天数" htmlFor="comment-ip-days">
        <InputNumber id="comment-ip-days" value={ipDays} onChange={setIpDays} min={1} max={36500} disabled={busy} />
      </Form.Item>
      <Form.Item label="审计日志保留天数" htmlFor="audit-days">
        <InputNumber id="audit-days" value={auditDays} onChange={setAuditDays} min={1} max={36500} disabled={busy} />
      </Form.Item>
      <Space>
        <Button type="primary" htmlType="submit" disabled={busy || conflict || !dirty}>保存保留期</Button>
        {conflict && <Button disabled={busy} onClick={() => void reload()}>重新加载保留期并放弃修改</Button>}
      </Space>
    </Form>}
  </section>;
}
