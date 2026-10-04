import { Alert, App as AntdApp, Button, Input, Modal, Select, Space, Typography } from "antd";
import { useEffect, useRef, useState } from "react";
import { postsApi } from "../api/posts";
import { pagesApi } from "../api/pages";
import type { ContentRevisionDetail, ContentRevisionSummary } from "../api/generated";
import { messageOf } from "../apiError";
import { formatDateTime } from "../timeZone";
import { useTimeZone } from "../timeZoneContext";

export function RevisionHistory({ kind, id, disabled, canRestore, onRestore }: {
  kind: "post" | "page"; id: string; disabled: boolean; canRestore: boolean;
  onRestore: (revision: string) => Promise<void>;
}) {
  const { modal } = AntdApp.useApp();
  const timeZone = useTimeZone();
  const api = kind === "post" ? postsApi : pagesApi;
  const [open, setOpen] = useState(false);
  const [rows, setRows] = useState<ContentRevisionSummary[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [detail, setDetail] = useState<ContentRevisionDetail | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const sequence = useRef(0);
  useEffect(() => () => { sequence.current++; }, []);
  async function show() {
    const request = ++sequence.current;
    setOpen(true); setBusy(true); setError(null); setRows([]); setSelected(null); setDetail(null);
    try { const rows = await api.revisions(id); if (request === sequence.current) setRows(rows); }
    catch (cause) { if (request === sequence.current) setError(messageOf(cause)); }
    finally { if (request === sequence.current) setBusy(false); }
  }
  async function select(revision: string) {
    const request = ++sequence.current;
    setSelected(revision); setDetail(null); setBusy(true); setError(null);
    try { const result = await api.revision(id, revision); if (request === sequence.current) setDetail(result); }
    catch (cause) { if (request === sequence.current) setError(messageOf(cause)); }
    finally { if (request === sequence.current) setBusy(false); }
  }
  function close() { sequence.current++; setOpen(false); setBusy(false); }
  function restore() {
    if (!selected) return;
    const revision = selected;
    modal.confirm({ title: "恢复这个历史版本到编辑稿？", content: "当前编辑区的未保存内容将被替换。已发布内容保持原样，核对后需要再次发布更新。首次发布后的固定链接不会恢复改名。", okText: "恢复到编辑稿",
      onOk: async () => {
        try { await onRestore(revision); close(); }
        catch (cause) { setError(messageOf(cause)); }
      } });
  }
  return <>
    <Button disabled={disabled} onClick={() => void show()} style={{ marginBottom: 16 }}>历史版本</Button>
    <Modal title="历史版本" open={open} onCancel={close} width={900} destroyOnHidden footer={<Space>
      <Button disabled={disabled || busy || detail === null || !canRestore} onClick={restore}>恢复到编辑稿</Button><Button onClick={close}>关闭</Button>
    </Space>}>
      <Typography.Paragraph type="secondary">保留最近 50 份保存记录（包括当前编辑稿）。迁移前的旧内容从首次修改时开始记录。</Typography.Paragraph>
      {error && <Alert type="error" showIcon title={error} />}
      <Select aria-label="选择历史版本" style={{ width: "100%", marginBottom: 12 }} value={selected} loading={busy} disabled={busy || disabled}
        placeholder={rows.length === 0 && !busy ? "尚无历史记录" : "选择一个版本查看"}
        onChange={value => void select(value)} options={rows.map(row => ({ value: row.id, label: `v${row.version} · ${formatDateTime(row.created_at, timeZone)} · ${row.title || "无标题"}` }))} />
      {detail && <><Typography.Title level={5}>{detail.title || "无标题"}</Typography.Title>
        <Typography.Paragraph type="secondary">/{detail.slug} · {detail.visibility === "public" ? "公开" : "私密"}{detail.excerpt ? ` · ${detail.excerpt}` : ""}</Typography.Paragraph>
        <Input.TextArea aria-label="历史正文" readOnly value={detail.content} rows={15} />
      </>}
    </Modal>
  </>;
}
