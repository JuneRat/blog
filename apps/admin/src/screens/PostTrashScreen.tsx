import { useEffect, useState } from "react";
import { ApiError, api, withRequestId } from "../api";
import { useAuth } from "../auth";
import { navigate, paths } from "../router";
import type { PostSummary } from "../types";

type TrashPage = { items: PostSummary[]; total: number; page: number; per_page: number };

function messageOf(error: unknown): string {
  return error instanceof ApiError ? withRequestId(error.message, error.requestId) : error instanceof Error ? error.message : "未知错误";
}

export function PostTrashScreen() {
  const { me } = useAuth();
  const [page, setPage] = useState(1);
  const [data, setData] = useState<TrashPage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const canPurge = me?.permissions.includes("post.purge") ?? false;

  useEffect(() => {
    let active = true;
    void api.listTrash(page).then((result) => { if (active) { setData(result); setError(null); } })
      .catch((e: unknown) => { if (active) setError(messageOf(e)); });
    return () => { active = false; };
  }, [page, revision]);

  async function act(post: PostSummary, purge: boolean) {
    if (purge && !window.confirm(`永久删除「${post.title || post.slug}」？文章、标签关联与系列位置将无法恢复。`)) return;
    setBusy(post.id);
    setError(null);
    try {
      if (purge) await api.purgePost(post.slug, post.version);
      else await api.restorePost(post.slug, post.version);
      setData(null);
      setRevision((value) => value + 1);
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(null);
    }
  }

  return <div className="screen">
    <header className="topbar"><h1>文章回收站</h1><div className="topbar-actions"><button type="button" className="button ghost" onClick={() => navigate(paths.list)}>返回文章</button></div></header>
    <p className="muted">恢复后的文章为草稿；原归档文章仍为归档，不会自动发布。永久删除不可撤销。</p>
    {error && <p className="error">{error}</p>}
    {data === null && !error && <p className="muted">正在加载…</p>}
    {data && <><p className="muted">共 {data.total} 篇</p><table className="posts"><thead><tr><th>标题</th><th>slug</th><th>原状态</th><th>版本</th><th>操作</th></tr></thead><tbody>
      {data.items.map((post) => <tr key={post.id}><td>{post.title || "（无标题）"}</td><td><code>{post.slug}</code></td><td>{post.status}</td><td>v{post.version}</td><td>
        <button type="button" disabled={busy !== null} onClick={() => void act(post, false)}>恢复</button>{" "}
        {canPurge && <button type="button" disabled={busy !== null} onClick={() => void act(post, true)}>永久删除</button>}
      </td></tr>)}
    </tbody></table><div className="topbar-actions"><button type="button" disabled={page <= 1} onClick={() => setPage(page - 1)}>上一页</button><span>第 {page} 页</span><button type="button" disabled={page * data.per_page >= data.total} onClick={() => setPage(page + 1)}>下一页</button></div></>}
  </div>;
}
