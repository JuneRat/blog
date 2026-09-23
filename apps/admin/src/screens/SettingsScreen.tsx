import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, settingsApi, themeSettingsApi, withRequestId } from "../api";
import { navigate, paths } from "../router";
import { codePointLength } from "../text";
import type { SiteSettings, ThemeSettings } from "../types";

function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    const base = error.status === 403 ? `没有权限：${error.message}` : error.message;
    return withRequestId(base, error.requestId);
  }
  return error instanceof Error ? error.message : "未知错误";
}

// 与后端 crates/domain/src/settings.rs 的 SITE_TITLE_MAX_CHARS /
// SITE_DESCRIPTION_MAX_CHARS 保持一致；单位是 Unicode 码点（见 ../text）。
const TITLE_MAX = 200;
const DESCRIPTION_MAX = 500;

interface Draft {
  title: string;
  description: string;
}

/**
 * 站点设置屏（site 分组：标题与描述）。
 *
 * - 读/写都需 `settings.manage`（后端判定；无权限时展示服务端错误）；
 * - 生效优先级：数据库 site 行 > 环境变量/默认值——`source` 字段标注当前来源，
 *   未配置时保存即「数据库接管」，此后环境变量调整不再影响站点；
 * - 保存携带 expected_version；409 `version_conflict` 走统一冲突流程：
 *   保留本地输入，提供「重新加载」（丢弃本地改动）与「仍然覆盖」（按服务器
 *   最新版本重新提交，二次确认），不自动重试、不静默覆盖；
 * - 保存响应期间表单仍可编辑：响应只规范化「未被继续编辑」的字段，等待期间
 *   的新输入原样保留，并提示尚未提交（否则新输入会丢失却显示「已保存」）；
 * - 长度上限按 Unicode 码点判定（见 `../text`），与后端 `chars().count()`
 *   同口径；因此不用 HTML `maxLength`（它按 UTF-16 代码单元截断）。
 */
export function SettingsScreen() {
  const [settings, setSettings] = useState<SiteSettings | null>(null);
  const [draft, setDraft] = useState<Draft>({ title: "", description: "" });
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /** 冲突流程：服务器最新视图（重新加载/仍然覆盖都基于它）。 */
  const [conflict, setConflict] = useState<SiteSettings | null>(null);

  /**
   * draft 的最新值镜像。
   *
   * 保存响应是异步的：回调里读闭包中的 draft 只能拿到「提交那一刻」的值，
   * 用它判断等待期间有没有新输入，会把新输入误判成未修改而覆盖掉。
   * 所有写 draft 的路径都经 setDraftValue，镜像与 state 不会失配。
   */
  const draftRef = useRef<Draft>(draft);

  const setDraftValue = useCallback((next: Draft) => {
    draftRef.current = next;
    setDraft(next);
  }, []);

  const apply = useCallback(
    (view: SiteSettings) => {
      setSettings(view);
      setDraftValue({ title: view.title, description: view.description });
    },
    [setDraftValue],
  );

  const load = useCallback(async (): Promise<SiteSettings | null> => {
    setError(null);
    try {
      const view = await settingsApi.get();
      apply(view);
      return view;
    } catch (e) {
      setError(messageOf(e));
      return null;
    }
  }, [apply]);

  useEffect(() => {
    void load();
  }, [load]);

  /**
   * 提交保存；`expectedVersion` 缺省为当前编辑基线（冲突覆盖时传服务器最新版）。
   *
   * 长度按 Unicode 码点判定，与后端 `str::chars().count()` 同一口径
   * （`String.length` 会把 emoji 算成 2，令前端比后端更早报错）。
   */
  const submit = useCallback(
    async (expectedVersion: number): Promise<void> => {
      // 提交快照：响应回来时逐字段比对，判断哪些字段在等待期间被继续编辑。
      const submitted = draftRef.current;
      const title = submitted.title.trim();
      if (title === "") {
        setError("站点标题不能为空。");
        return;
      }
      if (codePointLength(title) > TITLE_MAX) {
        setError(`站点标题长度不能超过 ${TITLE_MAX} 字符。`);
        return;
      }
      const description = submitted.description.trim();
      if (codePointLength(description) > DESCRIPTION_MAX) {
        setError(`站点描述长度不能超过 ${DESCRIPTION_MAX} 字符。`);
        return;
      }
      setError(null);
      setNotice(null);
      setBusy(true);
      try {
        const saved = await settingsApi.save({
          title,
          description,
          expected_version: expectedVersion,
        });
        // 服务器返回的是规范化后的值（trim 等）。它只覆盖「等待期间未被继续
        // 编辑」的字段；有新输入的字段保留本地值，否则这些输入会静默消失，
        // 界面却提示「已保存」。保留时明确提示尚有未提交内容。
        const current = draftRef.current;
        const merged: Draft = {
          title: current.title === submitted.title ? saved.title : current.title,
          description:
            current.description === submitted.description
              ? saved.description
              : current.description,
        };
        const pendingEdits =
          merged.title !== saved.title || merged.description !== saved.description;
        setSettings(saved);
        setDraftValue(merged);
        setConflict(null);
        setNotice(
          pendingEdits
            ? `已保存（v${saved.version}）；保存期间的新输入尚未提交，请再次保存。`
            : saved.source === "database"
              ? `已保存（v${saved.version}）。公开页面即刻使用新标题与描述。`
              : "已保存。",
        );
      } catch (e) {
        if (e instanceof ApiError && e.code === "version_conflict") {
          // 只取服务器最新视图供决策，不套用到表单——本地输入必须保留。
          try {
            const latest = await settingsApi.get();
            setConflict(latest);
            setError(null);
          } catch (reloadError) {
            setError(messageOf(reloadError));
          }
        } else {
          setError(messageOf(e));
        }
      } finally {
        setBusy(false);
      }
    },
    [setDraftValue],
  );

  const sourceHint =
    settings === null
      ? null
      : settings.source === "database"
        ? `当前生效来源：数据库（v${settings.version}）。`
        : "当前生效来源：环境变量/默认值（数据库尚未配置；保存后由数据库接管）。";

  // 计数按 trim 后的值算：后端也在 trim 后判长度，展示口径与提交口径一致。
  const titleCount = codePointLength(draft.title.trim());
  const descriptionCount = codePointLength(draft.description.trim());

  return (
    <div className="screen">
      <header className="topbar">
        <h1>站点设置</h1>
        <div className="topbar-actions">
          <button type="button" className="button ghost" onClick={() => navigate(paths.list)}>
            ← 返回列表
          </button>
        </div>
      </header>

      {error !== null && <p className="error">{error}</p>}
      {notice !== null && <p className="notice">{notice}</p>}
      {sourceHint !== null && <p className="muted">{sourceHint}</p>}
      {settings === null && error === null && <p className="muted">正在加载…</p>}

      {settings !== null && (
        <>
          <form
            className="settings-form"
            onSubmit={(event) => {
              event.preventDefault();
              if (busy || conflict !== null) return; // 冲突期间/提交中不重复提交
              void submit(settings.version);
            }}
          >
            {/* 计数字段在 <label> 之外：否则它会并入输入框的可访问名，
                读屏与 getByLabelText 都会看到「站点标题 6/200 字符」。 */}
            <div className="field">
              <label htmlFor="site-title">站点标题</label>
              <input
                id="site-title"
                value={draft.title}
                onChange={(event) => setDraftValue({ ...draft, title: event.target.value })}
                placeholder="显示在页头与浏览器标题"
                aria-invalid={titleCount > TITLE_MAX}
              />
              <span className={titleCount > TITLE_MAX ? "counter over-limit" : "counter"}>
                {titleCount}/{TITLE_MAX} 字符
              </span>
            </div>
            <div className="field">
              <label htmlFor="site-description">站点描述</label>
              <textarea
                id="site-description"
                value={draft.description}
                onChange={(event) =>
                  setDraftValue({ ...draft, description: event.target.value })
                }
                placeholder="一句话介绍这个站点（可留空）"
                aria-invalid={descriptionCount > DESCRIPTION_MAX}
                rows={3}
              />
              <span
                className={descriptionCount > DESCRIPTION_MAX ? "counter over-limit" : "counter"}
              >
                {descriptionCount}/{DESCRIPTION_MAX} 字符
              </span>
            </div>
            {conflict === null && (
              <button type="submit" className="button" disabled={busy}>
                保存
              </button>
            )}
          </form>

          {conflict !== null && (
            <div className="conflict">
              <p>
                设置已在别处被修改（服务器当前：v{conflict.version}「{conflict.title}」）。
                你的输入已保留，选择如何继续。
              </p>
              <div className="conflict-actions">
                <button
                  type="button"
                  className="button ghost"
                  disabled={busy}
                  onClick={() => {
                    // 重新加载：丢弃本地改动，采用服务器值。
                    apply(conflict);
                    setConflict(null);
                    setNotice("已重新加载服务器当前值。");
                  }}
                >
                  重新加载
                </button>
                <button
                  type="button"
                  className="button danger"
                  disabled={busy}
                  onClick={() => {
                    if (
                      window.confirm("仍然覆盖？将用你正在编辑的值替换服务器上的当前设置。")
                    ) {
                      void submit(conflict.version);
                    }
                  }}
                >
                  仍然覆盖
                </button>
              </div>
            </div>
          )}
        </>
      )}
      <ThemeSettingsForm />
    </div>
  );
}

function ThemeSettingsForm() {
  const [view, setView] = useState<ThemeSettings | null>(null);
  const [slug, setSlug] = useState("");
  const [conflict, setConflict] = useState<ThemeSettings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void themeSettingsApi.get().then((current) => {
      setView(current);
      setSlug(current.effective_slug);
    }).catch((cause) => setError(messageOf(cause)));
  }, []);

  async function save(expectedVersion: number) {
    const submitted = slug;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const saved = await themeSettingsApi.save(submitted, expectedVersion);
      setView(saved);
      setSlug((current) => current === submitted ? saved.effective_slug : current);
      setConflict(null);
      setNotice(`主题已切换为「${saved.available.find((item) => item.slug === saved.slug)?.name ?? saved.slug}」（v${saved.version}），公开页面即刻生效。`);
    } catch (cause) {
      if (cause instanceof ApiError && cause.code === "version_conflict") {
        try {
          setConflict(await themeSettingsApi.get());
        } catch (reloadError) {
          setError(messageOf(reloadError));
        }
      } else {
        setError(messageOf(cause));
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="settings-form" aria-label="主题设置">
      <h2>主题</h2>
      <p className="muted">选择已安装的主题。切换会同时更新公开页面和对应样式资源。</p>
      {error !== null && <p className="error">{error}</p>}
      {notice !== null && <p className="notice">{notice}</p>}
      {view === null && error === null && <p className="muted">正在加载主题…</p>}
      {view !== null && <>
        <p className="muted">当前主题：{view.available.find((item) => item.slug === view.effective_slug)?.name ?? view.effective_slug}（{view.source === "database" ? `数据库 v${view.version}` : "启动配置"}）</p>
        {view.slug !== view.effective_slug && <p className="error">已保存的主题「{view.slug}」当前未安装，公开页面暂用默认主题。请选择一个已安装主题并保存。</p>}
        <div className="field">
          <label htmlFor="active-theme">选择主题</label>
          <select id="active-theme" value={slug} disabled={busy} onChange={(event) => setSlug(event.target.value)}>
            {view.available.map((item) => <option key={item.slug} value={item.slug}>{item.name}</option>)}
          </select>
        </div>
        {conflict === null ? (
          <button type="button" className="button" disabled={busy || slug === view.slug} onClick={() => void save(view.version)}>切换主题</button>
        ) : (
          <div className="conflict">
            <p>主题已在别处被修改（服务器当前 v{conflict.version}）。你的选择已保留。</p>
            <div className="conflict-actions">
              <button type="button" className="button ghost" disabled={busy} onClick={() => { setView(conflict); setSlug(conflict.effective_slug); setConflict(null); }}>重新加载</button>
              <button type="button" className="button danger" disabled={busy} onClick={() => {
                if (window.confirm("仍然覆盖服务器当前主题？公开页面会立即切换。")) void save(conflict.version);
              }}>仍然覆盖</button>
            </div>
          </div>
        )}
      </>}
    </section>
  );
}
