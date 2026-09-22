import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, api, withRequestId } from "../api";
import { useAuth } from "../auth";
import { navigate, paths } from "../router";
import type { EditPageInput } from "../api";
import type { PageDetail, Visibility } from "../types";

interface FormState {
  slug: string;
  title: string;
  content: string;
  visibility: Visibility;
}

const EMPTY_FORM: FormState = {
  slug: "",
  title: "",
  content: "",
  visibility: "public",
};

function toForm(page: PageDetail): FormState {
  return {
    slug: page.slug,
    title: page.title,
    content: page.content,
    visibility: page.visibility,
  };
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
    left.content === right.content &&
    left.visibility === right.visibility
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

/** 见 PostEditScreen 的同名注释：避免请求返回时覆盖飞行途中的新输入。 */
function mergeServer(current: FormState, sent: FormState, server: FormState): FormState {
  return {
    slug: pickServer("slug", current, sent, server),
    title: pickServer("title", current, sent, server),
    content: pickServer("content", current, sent, server),
    visibility: pickServer("visibility", current, sent, server),
  };
}

/**
 * 只有版本冲突才提供「重新加载 / 仍然覆盖」两个动作。
 * 保留路径冲突、slug 占用等都是 400/409 里的其它码，重试无用。
 */
function isVersionConflict(error: unknown): boolean {
  if (!(error instanceof ApiError) || error.status !== 409) return false;
  return error.code === null || error.code === "version_conflict";
}

/**
 * 页面编辑屏。`slug === null` 表示新建。
 *
 * 与文章编辑器共享同一套交互（脏检测、按字段合并服务器响应、发布前先保存、
 * 改名后 replaceState），但 Page 没有作者、摘要与分类，页面权限是站点级。
 */
export function PageEditScreen({ slug }: { slug: string | null }) {
  const { me } = useAuth();
  const [form, setForm] = useState<FormState>(EMPTY_FORM);
  const [baseline, setBaseline] = useState<FormState>(EMPTY_FORM);
  const [version, setVersion] = useState<number | null>(null);
  const [pageStatus, setPageStatus] = useState<string>("draft");
  /** 当前表单内容所属的 slug（`applyServer` 写入）。用于识别「表单与地址不一致」。 */
  const [formSlug, setFormSlug] = useState<string | null>(null);
  const [loading, setLoading] = useState(slug !== null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);
  const formRef = useRef<FormState>(EMPTY_FORM);
  const baselineRef = useRef<FormState>(EMPTY_FORM);
  const loadedSlugRef = useRef<string | null>(null);

  function commitForm(next: FormState): void {
    formRef.current = next;
    setForm(next);
  }

  /** 用服务器响应同步表单，返回同步后是否仍有未保存改动。 */
  const applyServer = useCallback((page: PageDetail, sent?: FormState): boolean => {
    const server = toForm(page);
    const merged = sent === undefined ? server : mergeServer(formRef.current, sent, server);
    formRef.current = merged;
    baselineRef.current = server;
    loadedSlugRef.current = server.slug;
    setForm(merged);
    setBaseline(server);
    setFormSlug(server.slug);
    setVersion(page.version);
    setPageStatus(page.status);
    setConflict(false);
    return !formEquals(merged, server);
  }, []);

  /** 发布/撤回只改状态、不改正文：只同步状态类字段，表单原样保留。 */
  const applyStatus = useCallback((page: PageDetail): void => {
    setVersion(page.version);
    setPageStatus(page.status);
    setConflict(false);
  }, []);

  function hasUnsaved(): boolean {
    return !formEquals(formRef.current, baselineRef.current);
  }

  const dirty = !formEquals(form, baseline);
  /**
   * 表单内容与当前地址不一致：编辑页后退/切换到另一页时组件会被复用，
   * 若目标页加载失败（或仍在加载），表单里留着的仍是**上一篇**的内容。
   * 此时绝不能提交——`expected_version` 会用上一篇的版本号打到新 slug 上。
   * 用「表单所属 slug」而非「version 是否为空」判断，才能覆盖 A(已加载) → B(加载失败)。
   */
  const formMismatch = slug !== null && formSlug !== slug;
  /** 未加载成功（非加载中但表单仍不属于当前地址）：显示告警与重试入口。 */
  const unloaded = formMismatch && !loading;

  useEffect(() => {
    if (slug === null) {
      // 编辑页后退到新建页时组件会复用（App 不按 slug 加 key）；清空全部编辑状态，
      // 否则会带着上一篇的 slug/标题/正文/version 与「已发布」徽标去建新页。
      // 创建成功的 null → slug 跳转仍由 loadedSlugRef 保留已合并的新输入。
      formRef.current = EMPTY_FORM;
      baselineRef.current = EMPTY_FORM;
      loadedSlugRef.current = null;
      setForm(EMPTY_FORM);
      setBaseline(EMPTY_FORM);
      setFormSlug(null);
      setVersion(null);
      setPageStatus("draft");
      setBusy(false);
      setNotice(null);
      setError(null);
      setConflict(false);
      setLoading(false);
      return;
    }
    // 刚在本地同步过这个页面（例如改名后更新地址栏）：表单已经是最新的，不要重载覆盖。
    if (slug === loadedSlugRef.current) {
      setLoading(false);
      return;
    }
    let cancelled = false;
    void (async () => {
      setLoading(true);
      try {
        const page = await api.getPage(slug);
        if (!cancelled) applyServer(page);
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
    commitForm({ ...formRef.current, [key]: value });
  }

  /** 只在 slug 变化时提交 new_slug，避免把未改动值当作改名。 */
  function editPayload(): EditPageInput {
    const current = formRef.current;
    const trimmed = current.slug.trim();
    return {
      new_slug: slug !== null && trimmed.length > 0 && trimmed !== slug ? trimmed : undefined,
      title: current.title,
      content: current.content,
      visibility: current.visibility,
    };
  }

  async function save(): Promise<void> {
    setError(null);
    setNotice(null);
    // 用 formMismatch 而非 unloaded：加载途中也不能把上一篇的内容提交到新 slug。
    if (formMismatch) {
      setError("页面尚未成功加载，请先重新加载再保存，避免覆盖服务器内容。");
      return;
    }
    setBusy(true);
    try {
      if (slug === null) {
        const sent = formRef.current;
        const created = await api.createPage({
          slug: sent.slug.trim().length > 0 ? sent.slug.trim() : undefined,
          title: sent.title,
          content: sent.content,
          visibility: sent.visibility,
        });
        applyServer(created, sent);
        navigate(paths.editPage(created.slug));
        return;
      }
      const sent = formRef.current;
      const saved = await api.updatePage(slug, {
        ...editPayload(),
        expected_version: version ?? undefined,
      });
      const stillDirty = applyServer(saved, sent);
      if (saved.slug !== slug) {
        navigate(paths.editPage(saved.slug), { replace: true });
        return;
      }
      setNotice(
        stillDirty ? "已保存；等待期间的新改动尚未保存。" : "已保存（已发布页面直接更新线上）。",
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
      applyServer(await api.getPage(slug));
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
      const latest = await api.getPage(slug);
      const sent = formRef.current;
      const saved = await api.updatePage(slug, {
        ...editPayload(),
        expected_version: latest.version,
      });
      const stillDirty = applyServer(saved, sent);
      if (saved.slug !== slug) {
        navigate(paths.editPage(saved.slug), { replace: true });
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

  /** 发布/撤回：有未保存改动时先保存，再用保存得到的版本改状态。 */
  async function setPublished(publish: boolean): Promise<void> {
    // 表单不属于当前地址时不得改状态：没有可依据的版本号，等于盲写。
    if (slug === null || formMismatch) return;
    setError(null);
    setNotice(null);
    setBusy(true);
    const hadUnsavedEdits = dirty;
    let activeSlug = slug;
    try {
      let expected = version ?? undefined;
      if (hadUnsavedEdits) {
        const sent = formRef.current;
        const saved = await api.updatePage(activeSlug, {
          ...editPayload(),
          expected_version: expected,
        });
        activeSlug = saved.slug;
        expected = saved.version;
        applyServer(saved, sent);
      }
      const result = publish
        ? await api.publishPage(activeSlug, expected)
        : await api.unpublishPage(activeSlug, expected);
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
      if (activeSlug !== slug) {
        navigate(paths.editPage(activeSlug), { replace: true });
      }
      setBusy(false);
    }
  }

  // 发布与撤回是两个独立权限：合成一个按钮时必须按当前状态检查对应动作，
  // 否则只有 page.publish 的用户会看到一个点了就 403 的「撤回」按钮。
  const published = pageStatus === "published";
  const canToggle = me?.permissions.includes(published ? "page.unpublish" : "page.publish") ?? false;

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
          <button type="button" className="link" onClick={() => navigate(paths.pages)}>
            ← 返回页面列表
          </button>
          <h1>{slug === null ? "新建页面" : form.slug}</h1>
        </div>
        <div className="topbar-actions">
          <span className="badge">{pageStatus === "published" ? "已发布" : "草稿"}</span>
          {version !== null && <span className="muted">v{version}</span>}
          {pageStatus === "published" && slug !== null && (
            <a
              className="link"
              href={`/${encodeURIComponent(slug)}`}
              target="_blank"
              rel="noreferrer"
            >
              查看公开页面
            </a>
          )}
        </div>
      </header>

      {conflict && (
        <div className="conflict">
          <strong>内容已在别处修改。</strong> 你的编辑仍保留在下面，未被自动覆盖。
          <div className="conflict-actions">
            <button
              type="button"
              className="button"
              disabled={busy}
              onClick={() => void reloadFromServer()}
            >
              重新加载（丢弃本地改动）
            </button>
            <button
              type="button"
              className="button danger"
              disabled={busy}
              onClick={() => void overwriteWithLatest()}
            >
              仍然覆盖
            </button>
          </div>
        </div>
      )}

      {notice !== null && <p className="notice">{notice}</p>}
      {error !== null && <p className="error">{error}</p>}
      {unloaded && (
        <div className="warning">
          <strong>页面未能加载。</strong>{" "}
          可能已被删除或暂时不可达。已禁用保存与发布，避免把上一次打开的内容写到这个地址。
          <div className="conflict-actions">
            <button
              type="button"
              className="button"
              disabled={busy}
              onClick={() => void reloadFromServer()}
            >
              重新加载
            </button>
            <button
              type="button"
              className="button ghost"
              disabled={busy}
              onClick={() => navigate(paths.pages)}
            >
              返回页面列表
            </button>
          </div>
        </div>
      )}

      <form
        className="editor"
        onSubmit={(event) => {
          event.preventDefault();
          void save();
        }}
      >
        <label>
          slug（公开地址 /{form.slug || "…"}）
          <input
            value={form.slug}
            onChange={(event) => field("slug", event.target.value)}
            placeholder="留空则自动生成（发布后锁定；不能占用 admin、api 等系统路径）"
          />
        </label>
        <label>
          标题
          <input value={form.title} onChange={(event) => field("title", event.target.value)} />
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
          {canToggle && slug !== null && (
            <button
              type="button"
              className="button ghost"
              disabled={busy}
              onClick={() => void setPublished(pageStatus !== "published")}
            >
              {pageStatus === "published" ? "撤回为草稿" : "发布"}
            </button>
          )}
        </div>
      </form>
    </div>
  );
}
