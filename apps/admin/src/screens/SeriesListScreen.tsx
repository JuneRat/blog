import { useCallback, useEffect, useState } from "react";
import { ApiError, seriesApi, withRequestId } from "../api";
import { useAuth } from "../auth";
import { navigate, paths } from "../router";
import type { SeriesMemberRow, SeriesSummary } from "../types";

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
 * 系列管理屏：目录 CRUD + 成员顺序调整。
 *
 * - 管理需 series.manage；目录读取开放；
 * - 成员列表来自文章列表接口（按 series 过滤在前端完成——列表接口返回
 *   全部状态，含草稿/私密，它们保留位置但公开页不出现）；
 * - 上移/下移走整体重排接口：先交换再提交完整顺序与当前 series 版本；
 *   成功后用响应版本继续；403/409 原样展示（Author 不能重排他人文章）。
 */
export function SeriesListScreen() {
  const { me } = useAuth();
  const [series, setSeries] = useState<SeriesSummary[] | null>(null);
  const [members, setMembers] = useState<Record<string, SeriesMemberRow[]>>({});
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT);
  const canManage = me?.permissions.includes("series.manage") ?? false;

  const load = useCallback(async () => {
    setError(null);
    try {
      // 成员走专用目录端点：包含**其他作者**的成员（重排会改动它们的位置，
      // 无参 listPosts 只回当前作者的文章，多人系列会缺员并导致重排被拒）。
      const list = await seriesApi.list();
      const memberLists = await Promise.all(list.map((s) => seriesApi.members(s.slug)));
      setSeries(list);
      const bySeries: Record<string, SeriesMemberRow[]> = {};
      list.forEach((s, i) => {
        bySeries[s.id] = (memberLists[i] ?? []).sort(
          (a, b) => (a.series_order ?? 0) - (b.series_order ?? 0),
        );
      });
      setMembers(bySeries);
    } catch (e) {
      setSeries([]);
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
      await seriesApi.create({ name, slug });
      setDraft(EMPTY_DRAFT);
      setNotice(`已创建系列 ${name}。在文章编辑器中把文章加入系列。`);
      await load();
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function remove(s: SeriesSummary): Promise<void> {
    if (!window.confirm(`删除系列「${s.name}」（/series/${s.slug}）？`)) return;
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      await seriesApi.remove(s.slug, s.version);
      setNotice(`已删除系列 ${s.name}。`);
      await load();
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  /** 与相邻成员交换后提交完整顺序。 */
  async function move(s: SeriesSummary, index: number, delta: -1 | 1): Promise<void> {
    const list = members[s.id] ?? [];
    const target = index + delta;
    if (target < 0 || target >= list.length) return;
    const next = [...list];
    [next[index], next[target]] = [next[target], next[index]];
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      await seriesApi.reorder(
        s.slug,
        next.map((p) => p.id),
        s.version,
      );
      await load();
    } catch (e) {
      // 版本/权限/集合不一致：先重读目录（load 清错误位），再展示服务端原因。
      await load();
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="screen">
      <header className="topbar">
        <h1>系列</h1>
        <div className="topbar-actions">
          <button type="button" className="button ghost" onClick={() => navigate(paths.list)}>
            ← 返回列表
          </button>
        </div>
      </header>

      {error !== null && <p className="error">{error}</p>}
      {notice !== null && <p className="notice">{notice}</p>}
      {series === null && <p className="muted">正在加载…</p>}
      {series !== null && series.length === 0 && error === null && (
        <p className="muted">还没有系列。{canManage ? "在下方创建第一个。" : "需要持有系列管理权限的用户创建。"}</p>
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
              placeholder="如：Rust 入门系列"
            />
          </label>
          <label>
            slug
            <input
              value={draft.slug}
              onChange={(event) => setDraft({ ...draft, slug: event.target.value })}
              placeholder="如：rust-intro（创建后不可改）"
            />
          </label>
          <button type="submit" className="button" disabled={busy}>
            创建系列
          </button>
        </form>
      )}

      {series !== null &&
        series.map((s) => (
          <section key={s.id} className="series-block">
            <header className="topbar">
              <h2>
                <a href={`/series/${encodeURIComponent(s.slug)}`} target="_blank" rel="noreferrer">
                  {s.name}
                </a>{" "}
                <span className="muted">
                  {s.pub_post_count}/{s.post_count} 篇公开 · v{s.version}
                </span>
              </h2>
              {canManage && (
                <button
                  type="button"
                  className="button danger ghost"
                  disabled={busy}
                  onClick={() => void remove(s)}
                >
                  删除
                </button>
              )}
            </header>
            {(members[s.id] ?? []).length === 0 ? (
              <p className="muted">还没有文章加入这个系列。</p>
            ) : (
              <ol className="post-list series-admin-list">
                {(members[s.id] ?? []).map((post, index) => (
                  <li key={post.id} value={index + 1} className="post-item">
                    <div className="post-item-main">
                      <a href={`/admin/posts/${encodeURIComponent(post.slug)}/edit`}>{post.title || post.slug}</a>{" "}
                      <span className="muted">
                        {post.status === "published" ? "已发布" : post.status === "archived" ? "已归档" : "草稿"}
                        {post.author_id !== me?.user_id ? " · 他人文章" : ""}
                      </span>
                    </div>
                    {canManage && (
                      <div className="post-item-actions">
                        <button
                          type="button"
                          className="button ghost"
                          disabled={busy || index === 0}
                          onClick={() => void move(s, index, -1)}
                        >
                          ↑
                        </button>
                        <button
                          type="button"
                          className="button ghost"
                          disabled={busy || index === (members[s.id] ?? []).length - 1}
                          onClick={() => void move(s, index, 1)}
                        >
                          ↓
                        </button>
                      </div>
                    )}
                  </li>
                ))}
              </ol>
            )}
          </section>
        ))}
    </div>
  );
}
