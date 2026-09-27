import { Alert, App as AntdApp, Button, Form, Input, Select, Space, Typography } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useState } from "react";
import { ApiError, settingsApi, themeSettingsApi } from "../api";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { CoverPicker } from "../components/CoverPicker";
import { RetentionSettingsForm } from "../components/RetentionSettingsForm";
import { queryKeys } from "../queryClient";
import { codePointLength } from "../text";
import { useUnsavedGuard } from "../unsaved";
import type { SiteSettings, ThemeSettings } from "../types";

// 与后端 crates/domain/src/settings.rs 的 SITE_TITLE_MAX_CHARS /
// SITE_DESCRIPTION_MAX_CHARS 保持一致；单位是 Unicode 码点（见 ../text）。
const TITLE_MAX = 200;
const DESCRIPTION_MAX = 500;

interface Draft {
  title: string;
  description: string;
  /** 站点 logo 媒体 id（null = 无 logo）。 */
  logoMediaId: string | null;
}

const EMPTY_DRAFT: Draft = { title: "", description: "", logoMediaId: null };

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
 *
 * antd 迁移：值的唯一来源是 antd Form 的 store；`draft` 只是给计数字段用的
 * 渲染镜像，由 `onValuesChange`（同步回调）与 `writeDraft` 共同维护。响应回来
 * 时用 `readDraft()` 读 store，才能拿到「那一刻」的真实输入，保住上面那条
 * 「等待期间的新输入不被覆盖」的语义（与 PageEditScreen 同一套写法）。
 * 按钮的启用/禁用不依赖 `Form.useWatch`（rc-field-form 把 watch 通知批成宏任务，
 * 同步的 change→click 会点在仍禁用的按钮上），镜像 state 是同步更新的。
 */
export function SettingsScreen() {
  const [retentionDirty, setRetentionDirty] = useState(false);
  const { modal } = AntdApp.useApp();
  const { me } = useAuth();
  const canReadMedia = me?.permissions.includes("media.read") ?? false;
  const canUploadMedia = me?.permissions.includes("media.upload") ?? false;
  const queryClient = useQueryClient();
  const [formApi] = Form.useForm<Draft>();
  const [settings, setSettings] = useState<SiteSettings | null>(null);
  /** 表单值的渲染镜像（计数字段读它）。 */
  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT);
  const [actionError, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /** 冲突流程：服务器最新视图（重新加载/仍然覆盖都基于它）。 */
  const [conflict, setConflict] = useState<SiteSettings | null>(null);

  /** 站点设置查询：初次取到即写入表单（写操作后的新鲜度由失效重取负责）。 */
  const site = useQuery({
    queryKey: queryKeys.siteSettings(),
    queryFn: () => settingsApi.get(),
  });
  /** 取数失败与写操作失败分开：展示时动作错误优先。 */
  const errorText =
    actionError ?? (site.error === null ? null : permissionMessageOf(site.error));
  // 屏内多处直接设置文案（校验与写操作失败），沿用同一个 setter。
  const setError = setActionError;

  /** 读当前值。字段未被注册时可能缺失，用 EMPTY_DRAFT 补齐成完整的 Draft。 */
  const readDraft = useCallback(
    (): Draft => ({ ...EMPTY_DRAFT, ...formApi.getFieldsValue() }),
    [formApi],
  );

  /** 唯一的写入口：antd store 与渲染镜像一起更新（setFieldsValue 不触发 onValuesChange）。 */
  const writeDraft = useCallback(
    (next: Draft): void => {
      formApi.setFieldsValue(next);
      setDraft(next);
    },
    [formApi],
  );

  const apply = useCallback(
    (view: SiteSettings) => {
      setSettings(view);
      writeDraft({
        title: view.title,
        description: view.description,
        logoMediaId: view.logo_media_id,
      });
    },
    [writeDraft],
  );

  /** 表单镜像与服务器值的差异：用于离开确认（见 src/unsaved.tsx）。 */
  const dirty =
    settings !== null &&
    (draft.title !== settings.title ||
      draft.description !== settings.description ||
      draft.logoMediaId !== settings.logo_media_id);
  useUnsavedGuard(dirty || retentionDirty, "站点设置有未保存的修改，离开会丢失。");

  useEffect(() => {
    // 只在「还没有基线」时写入表单。保存响应要和「等待期间的新输入」逐字段合并
    // （见 submit），无条件 apply 会把用户刚输入的内容覆盖掉。
    if (site.data !== undefined && settings === null) apply(site.data);
  }, [site.data, settings, apply]);

  /**
   * 提交保存；`expectedVersion` 缺省为当前编辑基线（冲突覆盖时传服务器最新版）。
   *
   * 长度按 Unicode 码点判定，与后端 `str::chars().count()` 同一口径
   * （`String.length` 会把 emoji 算成 2，令前端比后端更早报错）。
   */
  const submit = useCallback(
    async (expectedVersion: number): Promise<void> => {
      // 提交快照：响应回来时逐字段比对，判断哪些字段在等待期间被继续编辑。
      const submitted = readDraft();
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
          // PUT 是整组替换：logo 始终显式提交（null = 确实要清除）。
          logo_media_id: submitted.logoMediaId,
          expected_version: expectedVersion,
        });
        // 服务器返回的是规范化后的值（trim 等）。它只覆盖「等待期间未被继续
        // 编辑」的字段；有新输入的字段保留本地值，否则这些输入会静默消失，
        // 界面却提示「已保存」。保留时明确提示尚有未提交内容。
        const current = readDraft();
        const merged: Draft = {
          title: current.title === submitted.title ? saved.title : current.title,
          description:
            current.description === submitted.description
              ? saved.description
              : current.description,
          logoMediaId:
            current.logoMediaId === submitted.logoMediaId
              ? saved.logo_media_id
              : current.logoMediaId,
        };
        const pendingEdits =
          merged.title !== saved.title ||
          merged.description !== saved.description ||
          merged.logoMediaId !== saved.logo_media_id;
        setSettings(saved);
        writeDraft(merged);
        setConflict(null);
        // 服务器已按新版本落库：让查询在后台失效重取，离开再回来时不会把旧缓存
        // 当成「刚刚从服务器读到」的基线。不 await：保存响应本身就是权威值，
        // 按钮不该为这次核对多转一会儿。
        void queryClient.invalidateQueries({ queryKey: queryKeys.siteSettings() });
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
          // 这里刻意不走 Query 缓存：需要的是「此刻服务器上的值」，
          // staleTime 内的缓存可能正是冲突的另一方。
          try {
            const latest = await settingsApi.get();
            setConflict(latest);
            setError(null);
          } catch (reloadError) {
            setError(permissionMessageOf(reloadError));
          }
        } else {
          setError(permissionMessageOf(e));
        }
      } finally {
        setBusy(false);
      }
    },
    [readDraft, writeDraft, queryClient],
  );

  /** 「仍然覆盖」：二次确认后按服务器最新版本重新提交，本地输入原样保留在表单里。 */
  function overwrite(expectedVersion: number): void {
    modal.confirm({
      title: "仍然覆盖？",
      content: "将用你正在编辑的值替换服务器上的当前设置。",
      okButtonProps: { danger: true },
      onOk: () => submit(expectedVersion),
    });
  }

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
    <>
      <Typography.Title level={3}>站点设置</Typography.Title>

      {errorText !== null && (
        <Alert type="error" showIcon title={errorText} style={{ marginBottom: 16 }} />
      )}
      {notice !== null && (
        <Alert type="success" showIcon title={notice} style={{ marginBottom: 16 }} />
      )}
      {sourceHint !== null && (
        <Typography.Paragraph type="secondary">{sourceHint}</Typography.Paragraph>
      )}
      {settings === null && errorText === null && (
        <Typography.Paragraph type="secondary">正在加载…</Typography.Paragraph>
      )}

      {settings !== null && (
        <>
          <Form
            form={formApi}
            layout="vertical"
            initialValues={EMPTY_DRAFT}
            onValuesChange={(_changed, all) => setDraft({ ...EMPTY_DRAFT, ...all })}
            onFinish={() => {
              if (busy || conflict !== null) return; // 冲突期间/提交中不重复提交
              void submit(settings.version);
            }}
            style={{ maxWidth: 720 }}
          >
            {/* 计数字段放在 Form.Item 的 extra 里，而不是 label 里：否则它会并入
                输入框的可访问名，读屏与 getByLabelText 都会看到「站点标题 6/200 字符」。 */}
            <Form.Item
              label="站点标题"
              name="title"
              extra={
                <Typography.Text type={titleCount > TITLE_MAX ? "danger" : "secondary"}>
                  {titleCount}/{TITLE_MAX} 字符
                </Typography.Text>
              }
            >
              <Input
                placeholder="显示在页头与浏览器标题"
                aria-invalid={titleCount > TITLE_MAX}
              />
            </Form.Item>
            <Form.Item
              label="站点描述"
              name="description"
              extra={
                <Typography.Text type={descriptionCount > DESCRIPTION_MAX ? "danger" : "secondary"}>
                  {descriptionCount}/{DESCRIPTION_MAX} 字符
                </Typography.Text>
              }
            >
              <Input.TextArea
                placeholder="一句话介绍这个站点（可留空）"
                aria-invalid={descriptionCount > DESCRIPTION_MAX}
                rows={3}
              />
            </Form.Item>
            {/*
              站点 logo：隐藏 Form.Item 只负责把 logoMediaId 注册进表单 store，
              真正控件是 CoverPicker；值由 draft 驱动、变化经 writeDraft 回写，
              与标题/描述的脏标记、冲突合并走同一条路径。
            */}
            <Form.Item name="logoMediaId" hidden>
              <Input />
            </Form.Item>
            <CoverPicker
              value={draft.logoMediaId}
              onChange={(id) => writeDraft({ ...readDraft(), logoMediaId: id })}
              currentUrl={
                draft.logoMediaId === (settings?.logo_media_id ?? null)
                  ? (settings?.logo_url ?? null)
                  : null
              }
              canReadMedia={canReadMedia}
              canUploadMedia={canUploadMedia}
              disabled={busy || conflict !== null}
              label="站点 logo"
            />
            {conflict === null && (
              // 用文案切换而不是 Button 的 loading：见 PostEditScreen 的同名说明。
              <Button type="primary" htmlType="submit" disabled={busy}>
                {busy ? "处理中…" : "保存"}
              </Button>
            )}
          </Form>

          {conflict !== null && (
            <Alert
              type="warning"
              showIcon
              title={`设置已在别处被修改（服务器当前：v${conflict.version}「${conflict.title}」）。`}
              description="你的输入已保留，选择如何继续。"
              style={{ marginBottom: 16 }}
              action={
                <Space>
                  <Button
                    disabled={busy}
                    onClick={() => {
                      // 重新加载：丢弃本地改动，采用服务器值。缓存也同步到该视图，
                      // 离开再回来时不会按旧缓存把它当成「未重新加载」。
                      apply(conflict);
                      queryClient.setQueryData(queryKeys.siteSettings(), conflict);
                      setConflict(null);
                      setNotice("已重新加载服务器当前值。");
                    }}
                  >
                    重新加载
                  </Button>
                  <Button danger disabled={busy} onClick={() => overwrite(conflict.version)}>
                    仍然覆盖
                  </Button>
                </Space>
              }
            />
          )}
        </>
      )}
      <ThemeSettingsForm />
      <RetentionSettingsForm onDirtyChange={setRetentionDirty} />
    </>
  );
}

function ThemeSettingsForm() {
  const { modal } = AntdApp.useApp();
  const queryClient = useQueryClient();
  const [themeForm] = Form.useForm<{ slug: string }>();
  const [view, setView] = useState<ThemeSettings | null>(null);
  /** 选中主题的渲染镜像，由 `onValuesChange`（同步回调）与 `writeSlug` 维护。 */
  const [slug, setSlug] = useState("");
  const [conflict, setConflict] = useState<ThemeSettings | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  /** 主题设置查询：初次取到写入表单（写操作后由失效重取保持缓存新鲜）。 */
  const theme = useQuery({
    queryKey: queryKeys.themeSettings(),
    queryFn: () => themeSettingsApi.get(),
  });
  /** 取数失败与写操作失败分开：展示时动作错误优先。 */
  const errorText =
    actionError ?? (theme.error === null ? null : permissionMessageOf(theme.error));
  const setError = setActionError;

  /** 读 store 里当前选中的主题（响应回来时用它判断等待期间是否换了选择）。 */
  const readSlug = useCallback((): string => themeForm.getFieldValue("slug") ?? "", [themeForm]);

  const writeSlug = useCallback(
    (next: string): void => {
      themeForm.setFieldsValue({ slug: next });
      setSlug(next);
    },
    [themeForm],
  );

  useEffect(() => {
    // 只在还没有视图时写入表单：保存响应会与「等待期间换的选择」比对后合并
    // （见 save），无条件写入会把用户的新选择改回去。
    if (theme.data !== undefined && view === null) {
      setView(theme.data);
      writeSlug(theme.data.effective_slug);
    }
  }, [theme.data, view, writeSlug]);

  async function save(expectedVersion: number) {
    const submitted = slug;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const saved = await themeSettingsApi.save(submitted, expectedVersion);
      setView(saved);
      // 等待期间换了别的主题：保留用户的新选择，不让响应把它改回去。
      if (readSlug() === submitted) writeSlug(saved.effective_slug);
      setConflict(null);
      // 切换已落库：后台失效重取（不 await，保存响应已是权威值；见站点设置同处说明）。
      void queryClient.invalidateQueries({ queryKey: queryKeys.themeSettings() });
      setNotice(
        `主题已切换为「${saved.available.find((item) => item.slug === saved.slug)?.name ?? saved.slug}」（v${saved.version}），公开页面即刻生效。`,
      );
    } catch (cause) {
      if (cause instanceof ApiError && cause.code === "version_conflict") {
        // 决策用的「此刻服务器值」同样绕开缓存：缓存可能正是冲突的一方。
        try {
          setConflict(await themeSettingsApi.get());
        } catch (reloadError) {
          setError(permissionMessageOf(reloadError));
        }
      } else {
        setError(permissionMessageOf(cause));
      }
    } finally {
      setBusy(false);
    }
  }

  /** 「仍然覆盖」：二次确认后按服务器最新版本重提。 */
  function overwrite(expectedVersion: number): void {
    modal.confirm({
      title: "仍然覆盖服务器当前主题？",
      content: "公开页面会立即切换。",
      okButtonProps: { danger: true },
      onOk: () => save(expectedVersion),
    });
  }

  return (
    <section aria-label="主题设置" style={{ marginTop: 32 }}>
      <Typography.Title level={4}>主题</Typography.Title>
      <Typography.Paragraph type="secondary">
        选择已安装的主题。切换会同时更新公开页面和对应样式资源。
      </Typography.Paragraph>
      {errorText !== null && (
        <Alert type="error" showIcon title={errorText} style={{ marginBottom: 16 }} />
      )}
      {notice !== null && (
        <Alert type="success" showIcon title={notice} style={{ marginBottom: 16 }} />
      )}
      {view === null && errorText === null && (
        <Typography.Paragraph type="secondary">正在加载主题…</Typography.Paragraph>
      )}
      <Form
        form={themeForm}
        layout="vertical"
        initialValues={{ slug: "" }}
        onValuesChange={(_changed, all) => setSlug(all.slug ?? "")}
        style={{ maxWidth: 360 }}
      >
        <Form.Item label="选择主题" name="slug">
          <Select
            loading={view === null}
            disabled={busy}
            options={(view?.available ?? []).map((item) => ({
              value: item.slug,
              label: item.name,
            }))}
          />
        </Form.Item>
      </Form>
      {view !== null && (
        <>
          <Typography.Paragraph type="secondary">
            当前主题：
            {view.available.find((item) => item.slug === view.effective_slug)?.name ??
              view.effective_slug}
            （{view.source === "database" ? `数据库 v${view.version}` : "启动配置"}）
          </Typography.Paragraph>
          {view.slug !== view.effective_slug && (
            <Alert
              type="error"
              showIcon
              title={`已保存的主题「${view.slug}」当前未安装，公开页面暂用默认主题。请选择一个已安装主题并保存。`}
              style={{ marginBottom: 16 }}
            />
          )}
          {conflict === null ? (
            <Button
              type="primary"
              disabled={busy || slug === view.slug}
              onClick={() => void save(view.version)}
            >
              切换主题
            </Button>
          ) : (
            <Alert
              type="warning"
              showIcon
              title={`主题已在别处被修改（服务器当前 v${conflict.version}）。`}
              description="你的选择已保留。"
              action={
                <Space>
                  <Button
                    disabled={busy}
                    onClick={() => {
                      // 重新加载：丢弃本地选择，采用服务器值（并同步缓存）。
                      setView(conflict);
                      writeSlug(conflict.effective_slug);
                      queryClient.setQueryData(queryKeys.themeSettings(), conflict);
                      setConflict(null);
                    }}
                  >
                    重新加载
                  </Button>
                  <Button danger disabled={busy} onClick={() => overwrite(conflict.version)}>
                    仍然覆盖
                  </Button>
                </Space>
              }
            />
          )}
        </>
      )}
    </section>
  );
}
