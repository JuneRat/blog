import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, api, categoryApi, seriesApi, withRequestId } from "../api";
import { useAuth } from "../auth";
import { navigate, paths } from "../router";
import type { EditPostInput } from "../api";
import type { CategorySummary, PostDetail, SeriesSummary, TagSummary, Visibility } from "../types";

interface FormState {
  slug: string;
  title: string;
  excerpt: string;
  content: string;
  visibility: Visibility;
  /** 选中的标签 id 集合（顺序无关；与服务器往返按集合比较）。 */
  tagIds: string[];
  /** 分类 id（null = 未分类）。 */
  categoryId: string | null;
  /** 系列 id（null = 不属于任何系列）。 */
  seriesId: string | null;
  /** 系列内序号（seriesId 非空时为正整数）。 */
  seriesOrder: string;
}

const EMPTY_FORM: FormState = {
  slug: "",
  title: "",
  excerpt: "",
  content: "",
  visibility: "public",
  tagIds: [],
  categoryId: null,
  seriesId: null,
  seriesOrder: "",
};

function toForm(post: PostDetail): FormState {
  return {
    slug: post.slug,
    title: post.title,
    excerpt: post.excerpt ?? "",
    content: post.content,
    visibility: post.visibility,
    tagIds: [...post.tag_ids],
    categoryId: post.category_id,
    seriesId: post.series_id,
    seriesOrder: post.series_order === null ? "" : String(post.series_order),
  };
}

/** 集合相等（标签选择顺序无关）。 */
function sameTags(left: string[], right: string[]): boolean {
  if (left.length !== right.length) return false;
  const set = new Set(left);
  return right.every((id) => set.has(id));
}

function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    const base = error.status === 403 ? `没有权限：${error.message}` : error.message;
    return withRequestId(base, error.requestId);
  }
  return error instanceof Error ? error.message : "未知错误";
}

function formEquals(left: FormState, right: FormState): boolean {
  return (
    left.slug === right.slug &&
    left.title === right.title &&
    left.excerpt === right.excerpt &&
    left.content === right.content &&
    left.visibility === right.visibility &&
    sameTags(left.tagIds, right.tagIds) &&
    left.categoryId === right.categoryId &&
    left.seriesId === right.seriesId &&
    left.seriesOrder.trim() === right.seriesOrder.trim()
  );
}

/** 只取用户在请求飞行期间没有改过的字段：改过的保留本地值，其余采用服务器值。 */
function pickServer<K extends keyof FormState>(
  key: K,
  current: FormState,
  sent: FormState,
  server: FormState,
): FormState[K] {
  return current[key] === sent[key] ? server[key] : current[key];
}

/**
 * 把服务器响应合并进当前表单。
 *
 * 请求发出后、响应返回前用户仍可能继续输入；直接整体回填服务器内容会把这些
 * 新输入静默丢掉。这里以「发出请求时的快照 `sent`」为基准逐字段判断：
 * 只有用户没动过的字段才接受服务器值，动过的字段保留本地编辑。
 */
function mergeServer(current: FormState, sent: FormState, server: FormState): FormState {
  // 标签是集合字段：用户没动过勾选才接受服务器值，动过则保留本地选择。
  const tags = sameTags(current.tagIds, sent.tagIds) ? server.tagIds : current.tagIds;
  const categoryId = current.categoryId === sent.categoryId ? server.categoryId : current.categoryId;
  const seriesChanged =
    current.seriesId !== sent.seriesId || current.seriesOrder.trim() !== sent.seriesOrder.trim();
  const seriesId = seriesChanged ? current.seriesId : server.seriesId;
  const seriesOrder = seriesChanged ? current.seriesOrder : server.seriesOrder;
  return {
    slug: pickServer("slug", current, sent, server),
    title: pickServer("title", current, sent, server),
    excerpt: pickServer("excerpt", current, sent, server),
    content: pickServer("content", current, sent, server),
    visibility: pickServer("visibility", current, sent, server),
    tagIds: tags,
    categoryId,
    seriesId,
    seriesOrder,
  };
}

/**
 * 只有版本冲突才提供「重新加载 / 仍然覆盖」两个动作。
 * slug 被占用等其它 409 用最新 version 重试仍然会失败，必须按普通错误展示。
 */
function isVersionConflict(error: unknown): boolean {
  if (!(error instanceof ApiError) || error.status !== 409) return false;
  // code 缺失时按旧契约（409 即版本冲突）保守处理。
  return error.code === null || error.code === "version_conflict";
}

/**
 * 编辑屏。`slug === null` 表示新建。
 *
 * 冲突策略（docs/content-lifecycle.md §1）：写入携带 expected_version；
 * 409 时**保留客户端编辑并提示处理**，不自动覆盖：
 * - 「重新加载」拉取服务器最新内容并丢弃本地改动；
 * - 「仍然覆盖」二次确认后，用**服务器最新 version** 重新提交本地内容。
 */
export function PostEditScreen({ slug }: { slug: string | null }) {
  const { me } = useAuth();
  const [form, setForm] = useState<FormState>(EMPTY_FORM);
  /** 最近一次与服务器同步的表单内容，用于判断是否有未保存编辑。 */
  const [baseline, setBaseline] = useState<FormState>(EMPTY_FORM);
  const [version, setVersion] = useState<number | null>(null);
  const [postStatus, setPostStatus] = useState<string>("draft");
  const [loading, setLoading] = useState(slug !== null);
  const [busy, setBusy] = useState(false);
  /** 标签目录（编辑器选择器）：已登录会话即可读。 */
  const [catalog, setCatalog] = useState<TagSummary[] | null>(null);
  /** 分类目录（编辑器选择器）。 */
  const [categoryCatalog, setCategoryCatalog] = useState<CategorySummary[] | null>(null);
  /** 系列目录（编辑器选择器）。 */
  const [seriesCatalog, setSeriesCatalog] = useState<SeriesSummary[] | null>(null);
  const [catalogError, setCatalogError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);
  /**
   * 表单与服务器基线的同步副本。异步请求返回时读它拿到「此刻正在编辑的内容」，
   * React state 在那个时点还是旧的闭包值。
   */
  const formRef = useRef<FormState>(EMPTY_FORM);
  const baselineRef = useRef<FormState>(EMPTY_FORM);
  /**
   * 当前表单对应的服务器 slug。改名成功后会先本地同步再更新地址栏；
   * 效果钩子据此跳过"同一篇文章"的重载，避免把刚合并好的编辑覆盖掉。
   */
  const loadedSlugRef = useRef<string | null>(null);

  function commitForm(next: FormState): void {
    formRef.current = next;
    setForm(next);
  }

  /**
   * 用服务器响应同步表单，返回同步后是否仍有未保存改动。
   *
   * 传入 `sent`（发出请求时的表单快照）时逐字段合并：请求飞行期间的新输入被保留。
   * 不传表示显式丢弃本地改动（初次加载、用户点「重新加载」）。
   */
  const applyServer = useCallback((post: PostDetail, sent?: FormState): boolean => {
    const server = toForm(post);
    const merged = sent === undefined ? server : mergeServer(formRef.current, sent, server);
    formRef.current = merged;
    baselineRef.current = server;
    loadedSlugRef.current = server.slug;
    setForm(merged);
    setBaseline(server);
    setVersion(post.version);
    setPostStatus(post.status);
    setConflict(false);
    return !formEquals(merged, server);
  }, []);

  /**
   * 发布/撤回只改状态、不改正文：只同步状态类字段，表单与脏标记原样保留。
   * 否则用「保存时的旧正文」回填会覆盖用户在发布请求飞行期间的新输入。
   */
  const applyStatus = useCallback((post: PostDetail): void => {
    setVersion(post.version);
    setPostStatus(post.status);
    setConflict(false);
  }, []);

  /** 此刻是否仍有未保存改动（读 ref，可用于 await 之后）。 */
  function hasUnsaved(): boolean {
    return !formEquals(formRef.current, baselineRef.current);
  }

  const dirty = !formEquals(form, baseline);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [tags, categories, seriesList] = await Promise.all([
          api.listTags(),
          categoryApi.list(),
          seriesApi.list(),
        ]);
        if (!cancelled) {
          setCatalog(tags);
          setCategoryCatalog(categories);
          setSeriesCatalog(seriesList);
        }
      } catch (e) {
        // 目录加载失败不阻塞正文编辑：只是暂时无法勾选标签。
        if (!cancelled) setCatalogError(messageOf(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (slug === null) {
      // 编辑页后退到新建页时组件会复用；清空全部编辑状态。
      // 创建成功的 null → slug 跳转仍由 loadedSlugRef 保留已合并的新输入。
      formRef.current = EMPTY_FORM;
      baselineRef.current = EMPTY_FORM;
      loadedSlugRef.current = null;
      setForm(EMPTY_FORM);
      setBaseline(EMPTY_FORM);
      setVersion(null);
      setPostStatus("draft");
      setBusy(false);
      setNotice(null);
      setError(null);
      setConflict(false);
      setLoading(false);
      return;
    }
    // 刚在本地同步过这篇文章（例如改名后更新地址栏）：表单已经是最新的，不要重载覆盖。
    if (slug === loadedSlugRef.current) {
      setLoading(false);
      return;
    }
    let cancelled = false;
    void (async () => {
      setLoading(true);
      try {
        const post = await api.getPost(slug);
        if (!cancelled) applyServer(post);
      } catch (e) {
        if (!cancelled) setError(messageOf(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [slug, applyServer]);

  function field<K extends keyof FormState>(key: K, value: FormState[K]): void {
    // 同步更新 ref：await 之后要用到「此刻」的内容，不能等 React 提交 state。
    commitForm({ ...formRef.current, [key]: value });
  }

  /** 只在 slug 变化时提交 new_slug，避免把未改动值当作改名；读 ref 取最新编辑。
   *  标签集合总是提交：后端按「整体替换」处理，未变化的集合是幂等重写。 */
  function editPayload(): EditPostInput {
    const current = formRef.current;
    const trimmed = current.slug.trim();
    return {
      new_slug: slug !== null && trimmed.length > 0 && trimmed !== slug ? trimmed : undefined,
      title: current.title,
      excerpt: current.excerpt,
      content: current.content,
      visibility: current.visibility,
      tag_ids: current.tagIds,
      category_id: current.categoryId,
      series: seriesPayload(current),
    };
  }

  /** 系列序号严格校验：正整数。Number("1.5") 不是整数、parseInt 不会截断，
   * 空白/0/负数/小数/非数字一律拒绝——非法值必须阻止提交而不是静默丢掉系列。 */
  function validSeriesOrder(value: string): boolean {
    const n = Number(value.trim());
    return Number.isInteger(n) && n > 0;
  }

  /** 系列载荷：未选系列 → null（退出）；选了系列 → 对象（调用前已通过校验）。 */
  function seriesPayload(current: FormState): { id: string; order: number } | null {
    if (current.seriesId === null) return null;
    return { id: current.seriesId, order: Number(current.seriesOrder.trim()) };
  }

  /** 勾选/取消一个标签（集合操作）。 */
  function toggleTag(tagId: string): void {
    const current = formRef.current;
    const next = current.tagIds.includes(tagId)
      ? current.tagIds.filter((id) => id !== tagId)
      : [...current.tagIds, tagId];
    commitForm({ ...current, tagIds: next });
  }

  async function save(): Promise<void> {
    // 系列序号是提交前提：非法值直接阻止，绝不静默丢弃系列选择。
    if (formRef.current.seriesId !== null && !validSeriesOrder(formRef.current.seriesOrder)) {
      setError("选择了系列时，系列内序号必须是正整数（如 1、2、3）。");
      return;
    }
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      if (slug === null) {
        const sent = formRef.current;
        const created = await api.createPost({
          slug: sent.slug.trim().length > 0 ? sent.slug.trim() : undefined,
          title: sent.title,
          excerpt: sent.excerpt.trim().length > 0 ? sent.excerpt : undefined,
          content: sent.content,
          visibility: sent.visibility,
          tag_ids: sent.tagIds,
          category_id: sent.categoryId ?? undefined,
          series: seriesPayload(sent) ?? undefined,
        });
        // 先本地同步（含创建期间的新输入），再更新地址；效果钩子会跳过重载。
        applyServer(created, sent);
        navigate(paths.editPost(created.slug));
        return;
      }
      const sent = formRef.current;
      const saved = await api.updatePost(slug, {
        ...editPayload(),
        expected_version: version ?? undefined,
      });
      // 合并而不是整体覆盖：请求飞行期间的新输入必须保留。
      const stillDirty = applyServer(saved, sent);
      if (saved.slug !== slug) {
        // 改名成功：先本地同步再替换地址栏，否则编辑器、发布按钮与公开链接
        // 会继续指向已不存在的旧地址，直到手动刷新。
        navigate(paths.editPost(saved.slug), { replace: true });
        return;
      }
      setNotice(
        stillDirty ? "已保存；等待期间的新改动尚未保存。" : "已保存（已发布内容直接更新线上）。",
      );
    } catch (e) {
      if (isVersionConflict(e)) {
        setConflict(true);
      } else {
        setError(messageOf(e));
      }
    } finally {
      setBusy(false);
    }
  }

  /** 冲突动作一：丢弃本地改动，重新加载服务器最新内容。 */
  async function reloadFromServer(): Promise<void> {
    if (slug === null) return;
    setError(null);
    setBusy(true);
    try {
      applyServer(await api.getPost(slug));
      setNotice("已重新加载服务器最新内容。");
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  /** 冲突动作二：二次确认后用服务器最新 version 覆盖本地内容。 */
  async function overwriteWithLatest(): Promise<void> {
    if (slug === null) return;
    if (!window.confirm("将用你当前的编辑内容覆盖服务器上的最新版本，确定继续？")) return;
    setError(null);
    setBusy(true);
    try {
      const latest = await api.getPost(slug);
      // 等待 getPost 期间的新输入也要一起提交，不能在覆盖时丢掉。
      const sent = formRef.current;
      const saved = await api.updatePost(slug, {
        ...editPayload(),
        expected_version: latest.version,
      });
      const stillDirty = applyServer(saved, sent);
      if (saved.slug !== slug) {
        navigate(paths.editPost(saved.slug), { replace: true });
        return;
      }
      setNotice(
        stillDirty ? "已覆盖保存；等待期间的新改动尚未保存。" : "已用服务器最新版本覆盖保存。",
      );
    } catch (e) {
      if (isVersionConflict(e)) {
        setConflict(true);
      } else {
        setError(messageOf(e));
      }
    } finally {
      setBusy(false);
    }
  }

  /**
   * 发布/撤回。这两个动作只改状态、不写正文：如果先把服务器返回的旧正文回填表单，
   * 本地未保存的编辑会被静默丢弃，线上发布的也仍是上一版内容。
   * 因此有未保存改动时先保存，再用保存得到的版本发布/撤回。
   */
  async function setPublished(publish: boolean): Promise<void> {
    if (slug === null) return;
    // 保存未存编辑走同一前提校验。
    if (formRef.current.seriesId !== null && !validSeriesOrder(formRef.current.seriesOrder)) {
      setError("选择了系列时，系列内序号必须是正整数（如 1、2、3）。");
      return;
    }
    setError(null);
    setNotice(null);
    setBusy(true);
    const hadUnsavedEdits = dirty;
    /** 保存若发生改名，后续发布必须打到新地址。 */
    let activeSlug = slug;
    try {
      let expected = version ?? undefined;
      if (hadUnsavedEdits) {
        const sent = formRef.current;
        const saved = await api.updatePost(activeSlug, {
          ...editPayload(),
          expected_version: expected,
        });
        activeSlug = saved.slug;
        expected = saved.version;
        applyServer(saved, sent);
      }
      // 发布/撤回不改正文：只同步状态，保留（可能还在变化的）表单。
      const result = publish
        ? await api.publishPost(activeSlug, expected)
        : await api.unpublishPost(activeSlug, expected);
      applyStatus(result);
      const stillDirty = hasUnsaved();
      if (publish) {
        if (stillDirty) setNotice("已发布；等待期间的新改动尚未保存。");
        else setNotice(hadUnsavedEdits ? "已保存并发布。" : "已发布。");
      } else {
        if (stillDirty) setNotice("已撤回为草稿；等待期间的新改动尚未保存。");
        else setNotice(hadUnsavedEdits ? "已保存并撤回为草稿。" : "已撤回为草稿。");
      }
    } catch (e) {
      if (isVersionConflict(e)) {
        setConflict(true);
      } else {
        setError(messageOf(e));
      }
    } finally {
      // 改名已落库：无论发布成功还是失败，地址都必须先指向新 slug，
      // 否则重试与「重新加载」都会打到已不存在的旧地址。
      if (activeSlug !== slug) {
        navigate(paths.editPost(activeSlug), { replace: true });
      }
      setBusy(false);
    }
  }

  const canPublish = me?.permissions.some((key) => key === "post.publish" || key === "post.publish_any") ?? false;

  if (loading) {
    return (
      <div className="screen">
        <p className="muted">正在加载…</p>
      </div>
    );
  }

  return (
    <div className="screen">
      <header className="topbar">
        <div>
          <button type="button" className="link" onClick={() => navigate(paths.list)}>
            ← 返回列表
          </button>
          <h1>{slug === null ? "新建草稿" : form.slug}</h1>
        </div>
        <div className="topbar-actions">
          <span className="badge">{postStatus === "published" ? "已发布" : "草稿"}</span>
          {version !== null && <span className="muted">v{version}</span>}
          {postStatus === "published" && slug !== null && (
            <a className="link" href={`/posts/${encodeURIComponent(slug)}`} target="_blank" rel="noreferrer">
              查看公开页面
            </a>
          )}
        </div>
      </header>

      {conflict && (
        <div className="conflict">
          <strong>内容已在别处修改。</strong> 你的编辑仍保留在下面，未被自动覆盖。
          <div className="conflict-actions">
            <button type="button" className="button" disabled={busy} onClick={() => void reloadFromServer()}>
              重新加载（丢弃本地改动）
            </button>
            <button type="button" className="button danger" disabled={busy} onClick={() => void overwriteWithLatest()}>
              仍然覆盖
            </button>
          </div>
        </div>
      )}

      {notice !== null && <p className="notice">{notice}</p>}
      {error !== null && <p className="error">{error}</p>}

      <form
        className="editor"
        onSubmit={(event) => {
          event.preventDefault();
          void save();
        }}
      >
        <label>
          slug
          <input
            value={form.slug}
            onChange={(event) => field("slug", event.target.value)}
            placeholder="留空则自动生成（发布后锁定）"
          />
        </label>
        <label>
          标题
          <input value={form.title} onChange={(event) => field("title", event.target.value)} />
        </label>
        <label>
          摘要
          <input value={form.excerpt} onChange={(event) => field("excerpt", event.target.value)} />
        </label>
        <label>
          可见性
          <select
            value={form.visibility}
            onChange={(event) => field("visibility", event.target.value as Visibility)}
          >
            <option value="public">公开</option>
            <option value="private">私有</option>
          </select>
        </label>
        <label>
          分类
          <select
            value={form.categoryId ?? ""}
            onChange={(event) =>
              commitForm({
                ...formRef.current,
                categoryId: event.target.value.length > 0 ? event.target.value : null,
              })
            }
          >
            <option value="">（未分类）</option>
            {(categoryCatalog ?? []).map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
        </label>
        <label>
          系列
          <select
            value={form.seriesId ?? ""}
            onChange={(event) =>
              commitForm({
                ...formRef.current,
                seriesId: event.target.value.length > 0 ? event.target.value : null,
              })
            }
          >
            <option value="">（不属于系列）</option>
            {(seriesCatalog ?? []).map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </select>
        </label>
        {form.seriesId !== null && (
          <label>
            系列内序号
            <input
              inputMode="numeric"
              value={form.seriesOrder}
              onChange={(event) =>
                commitForm({ ...formRef.current, seriesOrder: event.target.value })
              }
              placeholder="正整数；同一系列内唯一"
            />
          </label>
        )}
        <fieldset className="tag-picker">
          <legend>标签</legend>
          {catalogError !== null && <p className="error">{catalogError}</p>}
          {catalog === null && catalogError === null && <p className="muted">正在加载标签目录…</p>}
          {catalog !== null && catalog.length === 0 && (
            <p className="muted">
              还没有可用标签；先在<a href={paths.tags} onClick={(event) => { event.preventDefault(); navigate(paths.tags); }}>标签目录</a>创建。
            </p>
          )}
          {catalog !== null &&
            catalog.map((tag) => (
              <label key={tag.id} className="tag-option">
                <input
                  type="checkbox"
                  checked={form.tagIds.includes(tag.id)}
                  onChange={() => toggleTag(tag.id)}
                />
                {tag.name}
              </label>
            ))}
        </fieldset>
        <label>
          正文（Markdown）
          <textarea
            rows={18}
            value={form.content}
            onChange={(event) => field("content", event.target.value)}
          />
        </label>

        <div className="editor-actions">
          <button type="submit" className="button" disabled={busy}>
            {busy ? "处理中…" : "保存并更新线上"}
          </button>
          {canPublish && slug !== null && (
            <button
              type="button"
              className="button ghost"
              disabled={busy}
              onClick={() => void setPublished(postStatus !== "published")}
            >
              {postStatus === "published" ? "撤回为草稿" : "发布"}
            </button>
          )}
        </div>
      </form>
    </div>
  );
}
