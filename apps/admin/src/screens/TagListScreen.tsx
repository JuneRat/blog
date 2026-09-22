import { useCallback, useEffect, useState } from "react";
import { ApiError, api, withRequestId } from "../api";
import { useAuth } from "../auth";
import { navigate, paths } from "../router";
import type { TagSummary } from "../types";

function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    const base = error.status === 403 ? `没有权限：${error.message}` : error.message;
    return withRequestId(base, error.requestId);
  }
  return error instanceof Error ? error.message : "未知错误";
}

interface Draft {
  name: string;
  slug: string;
}

const EMPTY_DRAFT: Draft = { name: "", slug: "" };

/**
 * 标签目录管理屏。
 *
 * - 目录读取对全部已登录会话开放（编辑文章需要选标签）；
 * - 创建/改名/删除需 `tag.manage`（后端判定；无权限时界面隐藏管理入口）；
 * - 删除被引用的标签会被后端拒绝（409 `tag_in_use`，含草稿/私密引用），
 *   错误文案直接来自服务端（带引用规模），不前端猜测。
 */
export function TagListScreen() {
  const { me } = useAuth();
  const [tags, setTags] = useState<TagSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT);
  /** 正在改名的标签：editingId + 编辑值。 */
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editingName, setEditingName] = useState("");

  const canManage = me?.permissions.includes("tag.manage") ?? false;

  const load = useCallback(async () => {
    setError(null);
    try {
      setTags(await api.listTags());
    } catch (e) {
      setTags([]);
      setError(messageOf(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function create(): Promise<void> {
    const name = draft.name.trim();
    const slug = draft.slug.trim();
    if (name.length === 0 || slug.length === 0) {
      setError("名称与 slug 都不能为空。");
      return;
    }
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      await api.createTag({ name, slug });
      setDraft(EMPTY_DRAFT);
      setNotice(`已创建标签 ${name}。slug 创建后不可修改。`);
      await load();
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function rename(tag: TagSummary): Promise<void> {
    const name = editingName.trim();
    if (name.length === 0) {
      setError("名称不能为空。");
      return;
    }
    if (name === tag.name) {
      setEditingId(null);
      return;
    }
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      const updated = await api.renameTag(tag.slug, {
        name,
        expected_version: tag.version,
      });
      setNotice(`已改名；新版本 v${updated.version}，全部引用文章同步更新。`);
      setEditingId(null);
      await load();
    } catch (e) {
      // 版本冲突：目录已在别处被改。先重载目录（load 会清错误位），再展示冲突。
      setEditingId(null);
      await load();
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function remove(tag: TagSummary): Promise<void> {
    if (!window.confirm(`删除标签「${tag.name}」（/tags/${tag.slug}）？`)) return;
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      await api.deleteTag(tag.slug, tag.version);
      setNotice(`已删除标签 ${tag.name}。`);
      await load();
    } catch (e) {
      // tag_in_use 的服务端文案自带引用规模；直接展示，不掩盖为通用错误。
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="screen">
      <header className="topbar">
        <h1>标签</h1>
        <div className="topbar-actions">
          <button type="button" className="button ghost" onClick={() => navigate(paths.list)}>
            ← 返回列表
          </button>
        </div>
      </header>

      {error !== null && <p className="error">{error}</p>}
      {notice !== null && <p className="notice">{notice}</p>}
      {tags === null && <p className="muted">正在加载…</p>}
      {tags !== null && tags.length === 0 && error === null && (
        <p className="muted">还没有标签。{canManage ? "在下方创建第一个。" : "需要持有标签管理权限的用户创建。"}</p>
      )}

      {canManage && (
        <form
          className="tag-create"
          onSubmit={(event) => {
            event.preventDefault();
            void create();
          }}
        >
          <label>
            名称
            <input
              value={draft.name}
              onChange={(event) => setDraft({ ...draft, name: event.target.value })}
              placeholder="如：Rust"
            />
          </label>
          <label>
            slug
            <input
              value={draft.slug}
              onChange={(event) => setDraft({ ...draft, slug: event.target.value })}
              placeholder="如：rust（创建后不可改）"
            />
          </label>
          <button type="submit" className="button" disabled={busy}>
            创建标签
          </button>
        </form>
      )}

      {tags !== null && tags.length > 0 && (
        <table className="tags">
          <thead>
            <tr>
              <th>名称</th>
              <th>slug</th>
              <th>公开文章</th>
              <th>版本</th>
              {canManage && <th>操作</th>}
            </tr>
          </thead>
          <tbody>
            {tags.map((tag) => (
              <tr key={tag.id}>
                <td>
                  {editingId === tag.id ? (
                    <input
                      value={editingName}
                      autoFocus
                      onChange={(event) => setEditingName(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Enter") void rename(tag);
                        if (event.key === "Escape") setEditingId(null);
                      }}
                    />
                  ) : (
                    <a href={`/tags/${encodeURIComponent(tag.slug)}`} target="_blank" rel="noreferrer">
                      {tag.name}
                    </a>
                  )}
                </td>
                <td>
                  <code>{tag.slug}</code>
                </td>
                <td>{tag.public_post_count}</td>
                <td className="muted">v{tag.version}</td>
                {canManage && (
                  <td>
                    {editingId === tag.id ? (
                      <>
                        <button
                          type="button"
                          className="button ghost"
                          disabled={busy}
                          onClick={() => void rename(tag)}
                        >
                          保存
                        </button>
                        <button
                          type="button"
                          className="button ghost"
                          disabled={busy}
                          onClick={() => setEditingId(null)}
                        >
                          取消
                        </button>
                      </>
                    ) : (
                      <>
                        <button
                          type="button"
                          className="button ghost"
                          disabled={busy}
                          onClick={() => {
                            setEditingId(tag.id);
                            setEditingName(tag.name);
                          }}
                        >
                          改名
                        </button>
                        <button
                          type="button"
                          className="button danger ghost"
                          disabled={busy}
                          onClick={() => void remove(tag)}
                        >
                          删除
                        </button>
                      </>
                    )}
                  </td>
                )}
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
