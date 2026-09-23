import {
  Alert,
  App as AntdApp,
  Button,
  Flex,
  Form,
  Input,
  Select,
  Space,
  Tag,
  Typography,
} from "antd";
import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, api } from "../api";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import { navigate, paths } from "../router";
import { useUnsavedGuard } from "../unsaved";
import { MediaInsertPanel } from "../components/MediaInsertPanel";
import { useImageInsertion } from "../components/useImageInsertion";
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

/** antd `Input.TextArea` 的 ref 不是 DOM 节点，取出里面的原生 textarea 供插入逻辑使用。 */
type TextAreaHandle = { resizableTextArea?: { textArea: HTMLTextAreaElement } };

/**
 * 页面编辑屏。`slug === null` 表示新建。
 *
 * 与文章编辑器共享同一套交互（脏检测、按字段合并服务器响应、发布前先保存、
 * 改名后 replaceState），但 Page 没有作者、摘要与分类，页面权限是站点级。
 *
 * 值的唯一来源是 antd Form 的 store；`view` 只是给渲染与脏判断用的镜像，
 * 由 `onValuesChange`（同步回调）与写入函数共同维护，不另立第二份数据。
 * 这样 `pickServer`/`mergeServer` 仍能读到「响应回来那一刻」的真实输入，
 * 保留「请求飞行期间的新输入不被服务器响应覆盖」的既有语义。
 */
export function PageEditScreen({ slug }: { slug: string | null }) {
  const { me } = useAuth();
  const { modal } = AntdApp.useApp();
  const [formApi] = Form.useForm<FormState>();
  const [view, setView] = useState<FormState>(EMPTY_FORM);
  const [version, setVersion] = useState<number | null>(null);
  const [pageId, setPageId] = useState<string | null>(null);
  const [pageStatus, setPageStatus] = useState<string>("draft");
  /** 图片面板是否展开（编辑器内插入图片）。 */
  const [mediaOpen, setMediaOpen] = useState(false);
  /** 正文输入框：插入位置取自它的真实选区。 */
  const contentRef = useRef<HTMLTextAreaElement | null>(null);
  /** 当前表单内容所属的 slug（`applyServer` 写入）。用于识别「表单与地址不一致」。 */
  const [formSlug, setFormSlug] = useState<string | null>(null);
  const [loading, setLoading] = useState(slug !== null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);
  const [deleteConflict, setDeleteConflict] = useState(false);
  const baselineRef = useRef<FormState>(EMPTY_FORM);
  const loadedSlugRef = useRef<string | null>(null);

  const attachContentRef = useCallback((node: TextAreaHandle | null): void => {
    contentRef.current = node?.resizableTextArea?.textArea ?? null;
  }, []);

  /** 读当前值。字段未被注册时可能缺失，用 EMPTY_FORM 补齐成完整的 FormState。 */
  const readForm = useCallback(
    (): FormState => ({ ...EMPTY_FORM, ...formApi.getFieldsValue() }),
    [formApi],
  );

  /** 唯一的写入口：antd store 与渲染镜像一起更新（setFieldsValue 不触发 onValuesChange）。 */
  const writeForm = useCallback(
    (next: FormState): void => {
      formApi.setFieldsValue(next);
      setView(next);
    },
    [formApi],
  );

  /** 用服务器响应同步表单，返回同步后是否仍有未保存改动。 */
  const applyServer = useCallback(
    (page: PageDetail, sent?: FormState): boolean => {
      const server = toForm(page);
      const merged = sent === undefined ? server : mergeServer(readForm(), sent, server);
      baselineRef.current = server;
      loadedSlugRef.current = server.slug;
      writeForm(merged);
      setFormSlug(server.slug);
      setVersion(page.version);
      setPageId(page.id);
      setPageStatus(page.status);
      setConflict(false);
      setDeleteConflict(false);
      return !formEquals(merged, server);
    },
    [readForm, writeForm],
  );

  /** 发布/撤回只改状态、不改正文：只同步状态类字段，表单原样保留。 */
  const applyStatus = useCallback((page: PageDetail): void => {
    setVersion(page.version);
    setPageStatus(page.status);
    setConflict(false);
  }, []);

  function hasUnsaved(): boolean {
    return !formEquals(readForm(), baselineRef.current);
  }

  /**
   * 表单内容与当前地址不一致：编辑页后退/切换到另一页时组件会被复用，
   * 若目标页加载失败（或仍在加载），表单里留着的仍是**上一篇**的内容。
   * 此时绝不能提交——`expected_version` 会用上一篇的版本号打到新 slug 上。
   * 用「表单所属 slug」而非「version 是否为空」判断，才能覆盖 A(已加载) → B(加载失败)。
   */
  const formMismatch = slug !== null && formSlug !== slug;
  /** 未加载成功（非加载中但表单仍不属于当前地址）：显示告警与重试入口。 */
  const unloaded = formMismatch && !loading;
  /** 渲染镜像与最近一次服务器同步值的差异；用于离开确认（见 src/unsaved.tsx）。 */
  const dirty = !formEquals(view, baselineRef.current);
  useUnsavedGuard(dirty, "页面有未保存的修改，离开会丢失。");
  const queryClient = useQueryClient();
  /** 写成功后让页面列表失效：见 PostEditScreen 的同名说明（30s 缓存 + 关闭聚焦重取）。 */
  const invalidateList = useCallback((): void => {
    void queryClient.invalidateQueries({ queryKey: queryKeys.pages() });
  }, [queryClient]);

  useEffect(() => {
    if (slug === null) {
      // 编辑页后退到新建页时组件会复用（App 不按 slug 加 key）；清空全部编辑状态，
      // 否则会带着上一篇的 slug/标题/正文/version 与「已发布」徽标去建新页。
      // 创建成功的 null → slug 跳转仍由 loadedSlugRef 保留已合并的新输入。
      baselineRef.current = EMPTY_FORM;
      loadedSlugRef.current = null;
      writeForm(EMPTY_FORM);
      setFormSlug(null);
      setVersion(null);
      setPageId(null);
      setPageStatus("draft");
      setBusy(false);
      setNotice(null);
      setError(null);
      setConflict(false);
      setDeleteConflict(false);
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
        if (!cancelled) setError(permissionMessageOf(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [slug, applyServer, writeForm]);

  function commitContent(next: string): void {
    writeForm({ ...readForm(), content: next });
  }

  /** 只在 slug 变化时提交 new_slug，避免把未改动值当作改名。 */
  function editPayload(): EditPageInput {
    const current = readForm();
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
        const sent = readForm();
        const created = await api.createPage({
          slug: sent.slug.trim().length > 0 ? sent.slug.trim() : undefined,
          title: sent.title,
          content: sent.content,
          visibility: sent.visibility,
        });
        applyServer(created, sent);
        invalidateList();
        navigate(paths.editPage(created.slug));
        return;
      }
      const sent = readForm();
      const saved = await api.updatePage(slug, {
        ...editPayload(),
        expected_version: version ?? undefined,
      });
      const stillDirty = applyServer(saved, sent);
      invalidateList();
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
        setError(permissionMessageOf(e));
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
      setError(permissionMessageOf(e));
    } finally {
      setBusy(false);
    }
  }

  /** 冲突动作二：二次确认后用服务器最新 version 覆盖本地内容。 */
  function overwriteWithLatest(): void {
    if (slug === null) return;
    modal.confirm({
      title: "用当前编辑内容覆盖服务器上的最新版本？",
      content: "将用你当前的编辑内容覆盖服务器上的最新版本，确定继续？",
      okButtonProps: { danger: true },
      onOk: async () => {
        setError(null);
        setBusy(true);
        try {
          const latest = await api.getPage(slug);
          const sent = readForm();
          const saved = await api.updatePage(slug, {
            ...editPayload(),
            expected_version: latest.version,
          });
          const stillDirty = applyServer(saved, sent);
          invalidateList();
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
            setError(permissionMessageOf(e));
          }
        } finally {
          setBusy(false);
        }
      },
    });
  }

  /** 发布/撤回：有未保存改动时先保存，再用保存得到的版本改状态。 */
  async function setPublished(publish: boolean): Promise<void> {
    // 表单不属于当前地址时不得改状态：没有可依据的版本号，等于盲写。
    if (slug === null || formMismatch) return;
    setError(null);
    setNotice(null);
    setBusy(true);
    const hadUnsavedEdits = hasUnsaved();
    let activeSlug = slug;
    try {
      let expected = version ?? undefined;
      if (hadUnsavedEdits) {
        const sent = readForm();
        const saved = await api.updatePage(activeSlug, {
          ...editPayload(),
          expected_version: expected,
        });
        activeSlug = saved.slug;
        expected = saved.version;
        applyServer(saved, sent);
        /**
         * 先行保存已经改动了列表数据（标题/slug/版本）：**立刻失效**，不能等到
         * 状态切换成功——那一步失败时返回列表看到的还是保存前的数据。
         * 下面那次失效仍要保留：状态列（已发布/草稿）只有状态切换成功才变。
         */
        invalidateList();
      }
      const result = publish
        ? await api.publishPage(activeSlug, expected)
        : await api.unpublishPage(activeSlug, expected);
      applyStatus(result);
      invalidateList();
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
        setError(permissionMessageOf(e));
      }
    } finally {
      if (activeSlug !== slug) {
        navigate(paths.editPage(activeSlug), { replace: true });
      }
      setBusy(false);
    }
  }

  function deletePage(): void {
    if (slug === null || formMismatch || pageId === null || version === null) return;
    const current = readForm();
    modal.confirm({
      title: `永久删除页面「${current.title || slug}」？`,
      content: `地址 /${slug} 会立即失效。此操作没有回收站，无法恢复；未保存的修改也会丢失。`,
      okButtonProps: { danger: true },
      onOk: async () => {
        setBusy(true);
        setError(null);
        setNotice(null);
        setDeleteConflict(false);
        try {
          await api.deletePage(slug, pageId, version);
          invalidateList();
          navigate(paths.pages, { replace: true });
        } catch (cause) {
          if (isVersionConflict(cause)) {
            setError("页面已被修改。请重新加载并核对最新内容后再决定是否删除。");
            setDeleteConflict(true);
          } else {
            setError(permissionMessageOf(cause));
          }
        } finally {
          setBusy(false);
        }
      },
    });
  }

  // 发布与撤回是两个独立权限：合成一个按钮时必须按当前状态检查对应动作，
  // 否则只有 page.publish 的用户会看到一个点了就 403 的「撤回」按钮。
  const published = pageStatus === "published";
  const canReadMedia = me?.permissions.includes("media.read") ?? false;
  const canUploadMedia = me?.permissions.includes("media.upload") ?? false;
  /** 图片插入：拖入/粘贴上传与面板插入共用同一路径，插入只改本地表单。 */
  const insertion = useImageInsertion(
    contentRef,
    () => readForm().content,
    commitContent,
  );
  const canToggle = me?.permissions.includes(published ? "page.unpublish" : "page.publish") ?? false;
  const canDelete = me?.permissions.includes("page.delete") ?? false;

  if (loading) {
    return <Typography.Text type="secondary">正在加载…</Typography.Text>;
  }

  return (
    <>
      <Flex justify="space-between" align="center" wrap gap={12} style={{ marginBottom: 16 }}>
        <Flex align="center" gap={8}>
          <Typography.Title level={3} style={{ margin: 0 }}>
            {slug === null ? "新建页面" : view.slug}
          </Typography.Title>
          <Tag color={published ? "green" : undefined}>{published ? "已发布" : "草稿"}</Tag>
          {version !== null && <Typography.Text type="secondary">v{version}</Typography.Text>}
        </Flex>
        {published && slug !== null && (
          <Typography.Link href={`/${encodeURIComponent(slug)}`} target="_blank" rel="noreferrer">
            查看公开页面
          </Typography.Link>
        )}
      </Flex>

      {conflict && (
        <Alert
          type="warning"
          showIcon
          title="内容已在别处修改。"
          description="你的编辑仍保留在下面，未被自动覆盖。"
          style={{ marginBottom: 16 }}
          action={
            <Space>
              <Button disabled={busy} onClick={() => void reloadFromServer()}>
                重新加载（丢弃本地改动）
              </Button>
              <Button danger disabled={busy} onClick={overwriteWithLatest}>
                仍然覆盖
              </Button>
            </Space>
          }
        />
      )}

      {notice !== null && (
        <Alert type="success" showIcon title={notice} style={{ marginBottom: 16 }} />
      )}
      {error !== null && (
        <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />
      )}
      {deleteConflict && (
        <Button
          disabled={busy}
          onClick={() => void reloadFromServer()}
          style={{ marginBottom: 16 }}
        >
          重新加载页面
        </Button>
      )}
      {unloaded && (
        <Alert
          type="warning"
          showIcon
          title="页面未能加载。"
          description="可能已被删除或暂时不可达。已禁用保存与发布，避免把上一次打开的内容写到这个地址。"
          style={{ marginBottom: 16 }}
          action={
            <Button disabled={busy} onClick={() => void reloadFromServer()}>
              重新加载
            </Button>
          }
        />
      )}

      <Form
        form={formApi}
        layout="vertical"
        initialValues={EMPTY_FORM}
        onValuesChange={(_changed, all) => setView({ ...EMPTY_FORM, ...all })}
        onFinish={() => void save()}
      >
        <Form.Item label={`slug（公开地址 /${view.slug || "…"}）`} name="slug">
          <Input placeholder="留空则自动生成（发布后锁定；不能占用 admin、api 等系统路径）" />
        </Form.Item>
        <Form.Item label="标题" name="title">
          <Input />
        </Form.Item>
        <Form.Item label="可见性" name="visibility">
          <Select
            options={[
              { value: "public", label: "公开" },
              { value: "private", label: "私有" },
            ]}
          />
        </Form.Item>
        <Form.Item label="正文（Markdown）" name="content">
          <Input.TextArea
            ref={attachContentRef}
            rows={18}
            onDrop={(event) => {
              const files = Array.from(event.dataTransfer.files);
              if (files.length === 0) return;
              event.preventDefault();
              void insertion.insertFiles(files);
            }}
            onPaste={(event) => {
              const files = Array.from(event.clipboardData?.files ?? []);
              if (files.length === 0) return;
              event.preventDefault();
              void insertion.insertFiles(files);
            }}
          />
        </Form.Item>

        {canReadMedia && (
          <Flex gap={12} align="center" style={{ marginBottom: 16 }}>
            <Button
              type="link"
              onClick={() => {
                insertion.clear();
                setMediaOpen((open) => !open);
              }}
            >
              {mediaOpen ? "收起图片面板" : "插入图片"}
            </Button>
            <Typography.Text type="secondary">
              也可以把图片拖入正文框，或在正文框内粘贴剪贴板图片。
            </Typography.Text>
          </Flex>
        )}
        {insertion.error !== null && (
          <Alert type="error" showIcon title={insertion.error} style={{ marginBottom: 16 }} />
        )}
        {insertion.notice !== null && (
          <Alert type="success" showIcon title={insertion.notice} style={{ marginBottom: 16 }} />
        )}
        {mediaOpen && canReadMedia && (
          <div style={{ marginBottom: 16 }}>
            <MediaInsertPanel
              insertion={insertion}
              canUpload={canUploadMedia}
              onClose={() => setMediaOpen(false)}
            />
          </div>
        )}

        <Space>
          {/* 用文案切换而不是 Button 的 loading：见 PostEditScreen 的同名说明。 */}
          <Button type="primary" htmlType="submit" disabled={busy}>
            {busy ? "处理中…" : "保存并更新线上"}
          </Button>
          {canToggle && slug !== null && (
            <Button disabled={busy} onClick={() => void setPublished(pageStatus !== "published")}>
              {published ? "撤回为草稿" : "发布"}
            </Button>
          )}
          {canDelete && slug !== null && !formMismatch && pageId !== null && (
            <Button danger disabled={busy} onClick={deletePage}>
              永久删除页面
            </Button>
          )}
        </Space>
      </Form>
    </>
  );
}
