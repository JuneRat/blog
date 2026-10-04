import type { ContentPreviewResult } from "../api/generated";

const escapeAttribute = (text: string) => text.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;");

/** The opaque-origin iframe can execute registered plugin bundles, never inline
 * author scripts, and cannot access admin DOM, credentials or APIs. */
export function previewDocument(result: ContentPreviewResult, origin: string): string {
  const assets = `${new URL(origin).origin}/assets/plugins/`;
  const policy = `default-src 'none'; script-src ${assets}; style-src ${assets} 'unsafe-inline'; font-src ${assets}; img-src http: https: data: blob:; connect-src 'none'; base-uri 'none'; form-action 'none'`;
  return `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="${escapeAttribute(policy)}">
<meta name="referrer" content="no-referrer">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
:root { color-scheme: light dark; }
body { margin: 0; padding: 20px 24px; font: 15px/1.8 system-ui, sans-serif; overflow-wrap: anywhere; }
img { max-width: 100%; height: auto; }
pre { overflow-x: auto; padding: 12px; background: light-dark(#f5f5f5, #252525); border-radius: 6px; }
table { border-collapse: collapse; } th, td { border: 1px solid #8886; padding: 6px 12px; }
blockquote { margin-left: 0; border-left: 3px solid #8886; padding-left: 16px; }
a { color: light-dark(#1668dc, #69b1ff); }
</style>${result.head_html}</head><body><main data-content-root>${result.content_html}</main></body></html>`;
}
