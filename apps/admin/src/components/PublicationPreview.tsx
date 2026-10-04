import { Alert, Button, Modal, Space, Spin, Typography } from "antd";
import { useEffect, useRef, useState } from "react";
import { contentApi } from "../api/content";
import { messageOf } from "../apiError";
import { previewDocument } from "./previewDocument";

/** Explicit publication preview; local typing never submits content to this endpoint. */
export function PublicationPreview({ content, readContent, disabled = false }: {
  content: string;
  readContent: () => string;
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<{ source: string; document: string } | null>(null);
  const sequence = useRef(0);
  const latest = useRef(readContent);
  latest.current = readContent;
  useEffect(() => () => { sequence.current += 1; }, []);
  const stale = preview !== null && preview.source !== content;

  async function render() {
    const request = ++sequence.current;
    const source = latest.current();
    setOpen(true);
    setBusy(true);
    setPreview(null);
    setError(null);
    try {
      const result = await contentApi.previewContent(source);
      if (request !== sequence.current) return;
      if (latest.current() !== source) {
        setError("正文已变化，请重新预览最新内容。");
        return;
      }
      setPreview({ source, document: previewDocument(result, window.location.origin) });
    } catch (cause) {
      if (request === sequence.current) setError(messageOf(cause));
    } finally {
      if (request === sequence.current) setBusy(false);
    }
  }

  function close() {
    sequence.current += 1;
    setOpen(false);
    setBusy(false);
    setPreview(null);
  }

  return <>
    <Space wrap style={{ marginBottom: 16 }}>
      <Button disabled={disabled} onClick={() => void render()}>发布效果预览</Button>
      <Typography.Text type="secondary">核对当前插件和正文显示效果，不会保存或发布。</Typography.Text>
    </Space>
    <Modal title="发布效果预览" open={open} onCancel={close} width={960} destroyOnHidden
      footer={<Space><Button disabled={busy} onClick={() => void render()}>重新预览</Button><Button onClick={close}>关闭</Button></Space>}>
      <Typography.Paragraph type="secondary">使用当前站点的正文规则；文章标题、封面及完整主题排版请在公开页面查看。</Typography.Paragraph>
      {busy && <Spin tip="正在生成预览…"><div style={{ minHeight: 160 }} /></Spin>}
      {error && <Alert type="error" showIcon title={error} />}
      {stale && <Alert type="warning" showIcon title="正文已变化，请重新预览最新内容。" />}
      {preview && !stale && <iframe title="发布正文预览" sandbox="allow-scripts" referrerPolicy="no-referrer"
        srcDoc={preview.document} style={{ width: "100%", height: "65vh", border: 0 }} />}
    </Modal>
  </>;
}
