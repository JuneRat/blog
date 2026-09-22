import { useCallback, useEffect, useState } from "react";
import { ApiError, categoryApi, withRequestId } from "../api";
import { useAuth } from "../auth";
import { navigate, paths } from "../router";
import type { CategorySummary } from "../types";

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
  parent: string;
}

const EMPTY_DRAFT: Draft = { name: "", slug: "", parent: "" };

/**
 * 分类目录管理屏（树形缩进展示；管理需 category.manage，后端判定）。
 * 移动成环、删除保护（被引用/有子分类）的错误文案来自服务端。
 */
export function CategoryListScreen() {
  const { me } = useAuth();
  const [categories, setCategories] = useState<CategorySummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT);
  const canManage = me?.permissions.includes("category.manage") ?? false;

  const load = useCallback(async () => {
    setError(null);
    try {
      setCategories(await categoryApi.list());
    } catch (e) {
      setCategories([]);
      setError(messageOf(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /** 由 parent_id 计算缩进深度（目录小，直接逐层上溯）。 */
  function depthOf(cat: CategorySummary): number {
    const byId = new Map((categories ?? []).map((c) => [c.id, c]));
    let depth = 0;
    let cur = cat;
    while (cur.parent_id !== null && depth < 100) {
      const parent: CategorySummary | undefined = byId.get(cur.parent_id);
      if (parent === undefined) break;
      cur = parent;
      depth += 1;
    }
    return depth;
  }

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
      const parentSlug = draft.parent.trim();
      const parent = parentSlug.length > 0 ? parentSlug : undefined;
      await categoryApi.create({ name, slug, parent });
      setDraft(EMPTY_DRAFT);
      setNotice(`已创建分类 ${name}。slug 创建后不可修改。`);
      await load();
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function moveToRoot(cat: CategorySummary): Promise<void> {
    if (cat.parent_id === null) return;
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      await categoryApi.update(cat.slug, { name: cat.name, parent: null, expected_version: cat.version });
      setNotice(`已把「${cat.name}」移到根。`);
      await load();
    } catch (e) {
      // load 会清错误位：先重载目录，再展示服务端错误（成环/版本冲突）。
      await load();
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function moveUnder(cat: CategorySummary, parentSlug: string): Promise<void> {
    if (parentSlug === cat.slug) {
      setError("父分类不能是自身。");
      return;
    }
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      await categoryApi.update(cat.slug, { name: cat.name, parent: parentSlug, expected_version: cat.version });
      setNotice(`已把「${cat.name}」移到 ${parentSlug} 之下。`);
      await load();
    } catch (e) {
      // 同上：先重载再报错。
      await load();
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function remove(cat: CategorySummary): Promise<void> {
    if (!window.confirm(`删除分类「${cat.name}」（/categories/${cat.slug}）？`)) return;
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      await categoryApi.remove(cat.slug, cat.version);
      setNotice(`已删除分类 ${cat.name}。`);
      await load();
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="screen">
      <header className="topbar">
        <h1>分类</h1>
        <div className="topbar-actions">
          <button type="button" className="button ghost" onClick={() => navigate(paths.list)}>
            ← 返回列表
          </button>
        </div>
      </header>

      {error !== null && <p className="error">{error}</p>}
      {notice !== null && <p className="notice">{notice}</p>}
      {categories === null && <p className="muted">正在加载…</p>}
      {categories !== null && categories.length === 0 && error === null && (
        <p className="muted">还没有分类。{canManage ? "在下方创建第一个。" : "需要持有分类管理权限的用户创建。"}</p>
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
              placeholder="如：技术"
            />
          </label>
          <label>
            slug
            <input
              value={draft.slug}
              onChange={(event) => setDraft({ ...draft, slug: event.target.value })}
              placeholder="如：tech（创建后不可改）"
            />
          </label>
          <label>
            父分类
            <select value={draft.parent} onChange={(event) => setDraft({ ...draft, parent: event.target.value })}>
              <option value="">（根分类）</option>
              {(categories ?? []).map((c) => (
                <option key={c.id} value={c.slug}>
                  {c.name}
                </option>
              ))}
            </select>
          </label>
          <button type="submit" className="button" disabled={busy}>
            创建分类
          </button>
        </form>
      )}

      {categories !== null && categories.length > 0 && (
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
            {categories.map((cat) => (
              <tr key={cat.id}>
                <td style={{ paddingLeft: `${depthOf(cat) * 1.25}rem` }}>
                  <a href={`/categories/${encodeURIComponent(cat.slug)}`} target="_blank" rel="noreferrer">
                    {cat.name}
                  </a>
                </td>
                <td>
                  <code>{cat.slug}</code>
                </td>
                <td>{cat.pub_post_count}</td>
                <td className="muted">v{cat.version}</td>
                {canManage && (
                  <td>
                    <select
                      aria-label={`移动 ${cat.name}`}
                      value=""
                      disabled={busy}
                      onChange={(event) => {
                        const value = event.target.value;
                        if (value === "__root") void moveToRoot(cat);
                        else if (value.length > 0) void moveUnder(cat, value);
                      }}
                    >
                      <option value="">移动到…</option>
                      <option value="__root">（根分类）</option>
                      {(categories ?? [])
                        .filter((c) => c.id !== cat.id)
                        .map((c) => (
                          <option key={c.id} value={c.slug}>
                            {c.name}
                          </option>
                        ))}
                    </select>
                    <button
                      type="button"
                      className="button danger ghost"
                      disabled={busy}
                      onClick={() => void remove(cat)}
                    >
                      删除
                    </button>
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
