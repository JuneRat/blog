import { Alert, Button, Space, Typography, theme } from "antd";
import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { messageOf } from "../apiError";

/** Render unsaved Markdown with the same server rules as publication. */
export function ContentPreview({ content, disabled }: { content: string; disabled: boolean }) {
  const { token } = theme.useToken();
  const [html, setHtml] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const revision = useRef(0);
  useEffect(() => {
    revision.current += 1;
    setHtml(null);
    setError(null);
    setBusy(false);
    return () => { revision.current += 1; };
  }, [content]);
  async function preview() {
    const current = ++revision.current;
    setBusy(true);
    setError(null);
    try {
      const result = await api.previewContent(content);
      if (current === revision.current) setHtml(result.content_html);
    } catch (e) {
      if (current === revision.current) setError(messageOf(e));
    } finally {
      if (current === revision.current) setBusy(false);
    }
  }
  return <div style={{ marginBottom: 16 }}>
    <Space wrap align="center">
      <Button disabled={disabled || busy} onClick={() => void preview()}>{busy ? "正在预览…" : "预览正文"}</Button>
      {html !== null && <Button onClick={() => setHtml(null)}>收起预览</Button>}
      <Typography.Text type="secondary">预览当前输入，不保存或发布；展示正文，不含主题布局。</Typography.Text>
    </Space>
    {error && <Alert type="error" showIcon title={error} style={{ marginTop: 12 }} />}
    {html !== null && (
      <div
        aria-label="正文预览"
        style={{
          overflowWrap: "anywhere",
          overflowX: "auto",
          marginTop: 12,
          padding: "20px 24px",
          background: token.colorBgContainer,
          border: `1px solid ${token.colorBorderSecondary}`,
          borderRadius: token.borderRadiusLG,
          lineHeight: 1.8,
          fontSize: 15,
        }}
        dangerouslySetInnerHTML={{ __html: html }}
      />
    )}
  </div>;
}
