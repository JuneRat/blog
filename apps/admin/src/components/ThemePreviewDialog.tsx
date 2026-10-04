import { Alert, Modal, Spin, Typography } from "antd";
import { useEffect, useState } from "react";
import { themeSettingsApi } from "../api/settings";
import { messageOf } from "../apiError";

/** An inert document: no scripts, submissions, nested frames or remote assets. */
export function themePreviewDocument(html: string, origin: string): string {
  const doc = new DOMParser().parseFromString(html, "text/html");
  doc.querySelectorAll("script, base, meta[http-equiv], iframe, object, embed, link[rel=prefetch], link[rel=preload]").forEach(node => node.remove());
  const csp = doc.createElement("meta");
  csp.httpEquiv = "Content-Security-Policy";
  csp.content = `default-src 'none'; style-src ${origin} 'unsafe-inline'; img-src ${origin} data:; font-src ${origin}; base-uri ${origin}; form-action 'none'`;
  const base = doc.createElement("base");
  base.href = `${origin}/`;
  doc.head.prepend(csp, base);
  doc.body.setAttribute("inert", "");
  return `<!doctype html>${doc.documentElement.outerHTML}`;
}

export function ThemePreviewDialog({ theme, onClose }: {
  theme: { slug: string; name: string; release: string };
  onClose: () => void;
}) {
  const [document, setDocument] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    void themeSettingsApi.preview(theme.slug, theme.release).then(result => {
      if (!cancelled) setDocument(themePreviewDocument(result.html, window.location.origin));
    }).catch(cause => { if (!cancelled) setError(messageOf(cause)); });
    return () => { cancelled = true; };
  }, [theme.slug, theme.release]);
  return <Modal open title={`预览主题「${theme.name}」`} onCancel={onClose} footer={null} width={1100} destroyOnHidden>
    <Typography.Paragraph type="secondary">使用已保存的主题配置和当前公开文章预览首页，不会切换主题。预览禁用链接交互、脚本和统计。</Typography.Paragraph>
    {error && <Alert type="error" showIcon title={error} />}
    {document === null && !error && <Spin tip="正在生成主题预览…"><div style={{ minHeight: 180 }} /></Spin>}
    {document !== null && <iframe title="主题首页预览" sandbox="" referrerPolicy="no-referrer" srcDoc={document} style={{ width: "100%", height: "70vh", border: 0 }} />}
  </Modal>;
}
