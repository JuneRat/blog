import { useRef, useState } from 'react';
import { Alert, Button, Space } from 'antd';
import { commentsApi } from '../api';
import { permissionMessageOf } from '../apiError';

export function CommentEditor({ value, onChange, disabled = false }: { value: string; onChange: (value: string) => void; disabled?: boolean }) {
  const input = useRef<HTMLTextAreaElement>(null);
  const revision = useRef(0);
  const [html, setHtml] = useState<string>();
  const [error, setError] = useState<string>();
  const [previewing, setPreviewing] = useState(false);
  function change(value: string) { revision.current++; setHtml(undefined); onChange(value); }
  async function preview() {
    const current = ++revision.current;
    setPreviewing(true); setError(undefined);
    try { const result = await commentsApi.preview(value); if (revision.current === current) setHtml(result.content_html); }
    catch (e) { if (revision.current === current) setError(permissionMessageOf(e)); }
    finally { setPreviewing(false); }
  }
  return <Space orientation="vertical" style={{ width: '100%' }}>
    <textarea ref={input} disabled={disabled} aria-label="回复正文" value={value} maxLength={2000} rows={5}
      style={{ width: '100%', boxSizing: 'border-box', padding: 8, font: 'inherit' }} onChange={e => change(e.target.value)} />
    <Space wrap>
      {([
        ['粗体', '**', '**', '文字'], ['斜体', '*', '*', '文字'], ['代码', '`', '`', '代码'],
        ['链接', '[', '](https://example.com)', '链接文字'], ['引用', '\n> ', '', '引用'], ['列表', '\n- ', '', '项目'],
      ]).map(([label, before, after, fallback]) => <Button key={label} disabled={disabled} onClick={() => {
        const el = input.current;
        if (!el) return;
        const start = el.selectionStart, end = el.selectionEnd;
        const insertion = before + (value.slice(start, end) || fallback) + after;
        const next = value.slice(0, start) + insertion + value.slice(end);
        if (next.length > 2000) return;
        change(next);
        requestAnimationFrame(() => { el.focus(); el.setSelectionRange(start + insertion.length, start + insertion.length); });
      }}>{label}</Button>)}
      <Button disabled={disabled || !value.trim()} loading={previewing} onClick={() => void preview()}>预览</Button>
    </Space>
    {html !== undefined && <div aria-label="评论预览" style={{ overflowWrap: 'anywhere' }} dangerouslySetInnerHTML={{ __html: html }} />}
    {error && <Alert type="error" title={error} />}
  </Space>;
}
