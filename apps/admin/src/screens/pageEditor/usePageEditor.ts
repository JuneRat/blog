import { useCallback, useEffect, useRef, useState } from "react";
import { App as AntdApp, Form } from "antd";
import { useQueryClient } from "@tanstack/react-query";
import { useEditorRequestGuard } from "../../useEditorRequestGuard";
import { useConflictSnapshot } from "../../useConflictSnapshot";
import { useLocalDraft } from "../../localDraft";
import { invalidateAfterWrite } from "../../queryEffects";
import { statusLabel, type ContentAction } from "../../components/ContentLifecycleControls";
import { ApiError } from "../../api/client";
import { pagesApi } from "../../api/pages";
import type { EditPageInput } from "../../api/generated";
import type { PageDetail } from "../../types";
import { permissionMessageOf } from "../../apiError";
import { useAuth } from "../../auth";
import { navigate, paths } from "../../router";
import { useUnsavedGuard } from "../../unsaved";
import { useImageInsertion } from "../../components/useImageInsertion";
import { EMPTY_FORM, toForm, formEquals, mergeServer, type FormState } from "./form";

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

/** Page request lifecycle and server baseline; editable values remain in the Form store. */
export function usePageEditor(id: string | null) {
  const beginRequest = useEditorRequestGuard(id);
  const { me } = useAuth();
  const { modal } = AntdApp.useApp();
  const [formApi] = Form.useForm<FormState>();
  const [view, setView] = useState<FormState>(EMPTY_FORM);
  const [version, setVersion] = useState<number | null>(null);
  const [pageId, setPageId] = useState<string | null>(null);
  const [pageStatus, setPageStatus] = useState<string>("draft");
  const [publishedAt, setPublishedAt] = useState<string | null>(null);
  /** 图片面板是否展开（编辑器内插入图片）。 */
  const [mediaOpen, setMediaOpen] = useState(false);
  /** 正文输入框：插入位置取自它的真实选区。 */
  const contentRef = useRef<HTMLTextAreaElement | null>(null);
  const [loading, setLoading] = useState(id !== null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);
  const comparison = useConflictSnapshot({ active: conflict, id, load: pagesApi.getPage });
  const localSavedRef = useRef<(id: string, version: number, merged: FormState, dirty: boolean) => void>(() => {});
  const [deleteConflict, setDeleteConflict] = useState(false);
  const baselineRef = useRef<FormState>(EMPTY_FORM);
  const loadedIdRef = useRef<string | null>(null);

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
      loadedIdRef.current = page.id;
      writeForm(merged);
      setVersion(page.version);
      setPageId(page.id);
      setPageStatus(page.status);
      setPublishedAt(page.published_at);
      setConflict(false);
      setDeleteConflict(false);
      const stillDirty = !formEquals(merged, server);
      if (sent !== undefined) localSavedRef.current(page.id, page.version, merged, stillDirty);
      return stillDirty;
    },
    [readForm, writeForm],
  );

  /** 发布/撤回只改状态、不改正文：只同步状态类字段，表单原样保留。 */
  const applyStatus = useCallback((page: PageDetail): void => {
    setVersion(page.version);
    setPageStatus(page.status);
    setPublishedAt(page.published_at);
    setConflict(false);
  }, []);

  function hasUnsaved(): boolean {
    return !formEquals(readForm(), baselineRef.current);
  }

  /**
   * 表单内容与当前地址不一致：编辑页后退/切换到另一页时组件会被复用，
   * 若目标页加载失败（或仍在加载），表单里留着的仍是**上一篇**的内容。
   * 此时绝不能提交——`expected_version` 会用上一篇的版本号打到另一个 ID 上。
   * 用「表单所属 ID」而非「version 是否为空」判断，才能覆盖 A(已加载) → B(加载失败)。
   */
  const formMismatch = id !== null && pageId !== id;
  /** 未加载成功（非加载中但表单仍不属于当前地址）：显示告警与重试入口。 */
  const unloaded = formMismatch && !loading;
  /** 渲染镜像与最近一次服务器同步值的差异；用于离开确认（见 src/unsaved.tsx）。 */
  const dirty = !formEquals(view, baselineRef.current);
  useUnsavedGuard(
    () => !formMismatch && !loading && hasUnsaved(),
    "页面有未保存的修改，离开会丢失。",
  );
  const localDraft = useLocalDraft({
    owner: me?.user_id, kind: "page", id,
    ready: !loading && !formMismatch && (id !== null || pageId === null),
    disabled: busy,
    dirty, value: view, template: EMPTY_FORM, baselineVersion: version,
    readValue: readForm, readDirty: hasUnsaved,
    onRestore: (value, savedVersion) => {
      writeForm(value);
      if (id !== null && savedVersion !== version) {
        // Restored input still belongs to its original baseline, never to the freshly loaded version.
        setVersion(savedVersion);
        setConflict(true);
        comparison.refresh();
      }
    },
  });
  localSavedRef.current = localDraft.saved;

  const queryClient = useQueryClient();
  /** 同时刷新页面、回收站与媒体使用位置；表单始终以提交响应为基线。 */
  const invalidateRelated = useCallback((): void => {
    void invalidateAfterWrite(queryClient, "page");
  }, [queryClient]);

  useEffect(() => {
    setBusy(false);
    setNotice(null);
    setError(null);
    setConflict(false);
    if (id === null) {
      // 编辑页后退到新建页时组件会复用（App 不按 ID 加 key）；清空全部编辑状态，
      // 否则会带着上一篇的 slug/标题/正文/version 与「已发布」徽标去建新页。
      // 创建成功的 null → ID 跳转仍由 loadedIdRef 保留已合并的新输入。
      baselineRef.current = EMPTY_FORM;
      loadedIdRef.current = null;
      writeForm(EMPTY_FORM);
      setVersion(null);
      setPageId(null);
      setPageStatus("draft");
      setPublishedAt(null);
      setBusy(false);
      setNotice(null);
      setError(null);
      setConflict(false);
      setDeleteConflict(false);
      setLoading(false);
      return;
    }
    // 刚创建并同步过这个页面：切到新 ID 时不要重载覆盖请求期间的新输入。
    if (id === loadedIdRef.current) {
      setLoading(false);
      return;
    }
    let cancelled = false;
    void (async () => {
      setLoading(true);
      try {
        const page = await pagesApi.getPage(id);
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
  }, [id, applyServer, writeForm]);

  function commitContent(next: string): void {
    writeForm({ ...readForm(), content: next });
  }

  /** 只在 slug 变化时提交 new_slug，避免把未改动值当作改名。 */
  function editPayload(): EditPageInput {
    const current = readForm();
    const trimmed = current.slug.trim();
    return {
      new_slug: id !== null && trimmed.length > 0 && trimmed !== baselineRef.current.slug ? trimmed : undefined,
      title: current.title,
      content: current.content,
      visibility: current.visibility,
    };
  }

  async function save(): Promise<void> {
    const isCurrent = beginRequest();
    if (localDraft.blocksEditing) {
      setError("请先恢复或丢弃当前窗口的本机副本。");
      return;
    }
    setError(null);
    setNotice(null);
    // 用 formMismatch 而非 unloaded：加载途中也不能把上一篇的内容提交到另一个 ID。
    if (formMismatch) {
      setError("页面尚未成功加载，请先重新加载再保存，避免覆盖服务器内容。");
      return;
    }
    setBusy(true);
    try {
      if (id === null) {
        const sent = readForm();
        const created = await pagesApi.createPage({
          slug: sent.slug.trim().length > 0 ? sent.slug.trim() : undefined,
          title: sent.title,
          content: sent.content,
          visibility: sent.visibility,
        });
        invalidateRelated();
        if (!isCurrent()) return;
        applyServer(created, sent);
        navigate(paths.editPage(created.id));
        return;
      }
      const sent = readForm();
      const saved = await pagesApi.updatePage(id, {
        ...editPayload(),
        expected_version: version ?? undefined,
      });
      invalidateRelated();
      if (!isCurrent()) return;
      const stillDirty = applyServer(saved, sent);

      setNotice(
        stillDirty ? "已保存；等待期间的新改动尚未保存。" : "已保存。",
      );
    } catch (e) {
      if (!isCurrent()) return;
      if (isVersionConflict(e)) {
        setConflict(true);
        comparison.refresh();
      } else {
        setError(permissionMessageOf(e));
      }
    } finally {
      if (isCurrent()) setBusy(false);
    }
  }

  /** 冲突动作一：丢弃本地改动，重新加载服务器最新内容。 */
  async function reloadFromServer(): Promise<void> {
    const isCurrent = beginRequest();
    if (id === null) return;
    setError(null);
    setBusy(true);
    try {
      const result = await pagesApi.getPage(id);
      if (!isCurrent()) return;
      applyServer(result);
      setNotice("已重新加载服务器最新内容。");
    } catch (e) {
      if (!isCurrent()) return;
      setError(permissionMessageOf(e));
    } finally {
      if (isCurrent()) setBusy(false);
    }
  }

  /** 冲突动作二：只覆盖已展示的服务器版本，发生新提交时仍拒绝覆盖。 */
  function overwriteWithLatest(): void {
    const isCurrent = beginRequest();
    if (id === null || comparison.snapshot === null) return;
    const latest = comparison.snapshot;
    modal.confirm({
      title: `用当前编辑内容覆盖已展示的服务器版本 v${latest.version}？`,
      content: "请先核对差异。若服务器再次变化，本次覆盖会被拒绝，本地输入会保留。",
      okButtonProps: { danger: true },
      onOk: async () => {
        if (!isCurrent()) return;
        setError(null);
        setBusy(true);
        try {
          const sent = readForm();
          const saved = await pagesApi.updatePage(id, {
            ...editPayload(),
            expected_version: latest.version,
          });
          invalidateRelated();
          if (!isCurrent()) return;
          const stillDirty = applyServer(saved, sent);

          setNotice(
            stillDirty ? "已覆盖保存；等待期间的新改动尚未保存。" : "已用服务器最新版本覆盖保存。",
          );
        } catch (e) {
          if (!isCurrent()) return;
          if (isVersionConflict(e)) {
            setConflict(true);
            comparison.refresh();
          } else {
            setError(permissionMessageOf(e));
          }
        } finally {
          if (isCurrent()) setBusy(false);
        }
      },
    });
  }

  /** 发布/预约先保存；撤回/归档保留本地编辑，不将其写入线上。 */
  async function changeStatus(action: ContentAction, at?: string): Promise<void> {
    const isCurrent = beginRequest();
    // 表单不属于当前地址时不得改状态：没有可依据的版本号，等于盲写。
    if (id === null || formMismatch) return;
    setError(null);
    setNotice(null);
    setBusy(true);
    const hadUnsavedEdits = hasUnsaved();
    const savesContent = action === "publish" || action === "schedule";
    try {
      let expected = version ?? undefined;
      if (savesContent && hadUnsavedEdits && pageStatus !== "archived") {
        const sent = readForm();
        const saved = await pagesApi.updatePage(id, {
          ...editPayload(),
          expected_version: expected,
        });
        invalidateRelated();
        if (!isCurrent()) return;
        expected = saved.version;
        applyServer(saved, sent);
        /**
         * 先行保存已经改动了列表数据（标题/slug/版本）：**立刻失效**，不能等到
         * 状态切换成功——那一步失败时返回列表看到的还是保存前的数据。
         * 下面那次失效仍要保留：状态列（已发布/草稿）只有状态切换成功才变。
         */
      }
      const result = action === "publish"
        ? await pagesApi.publishPage(id, expected)
        : action === "schedule" ? await pagesApi.schedulePage(id, at!, expected)
        : action === "archive" ? await pagesApi.archivePage(id, expected)
        : await pagesApi.unpublishPage(id, expected);
      invalidateRelated();
      if (!isCurrent()) return;
      applyStatus(result);
      setNotice(`状态已更新为${statusLabel(result.status)}。${hasUnsaved() ? "还有未保存的改动。" : ""}`);
    } catch (e) {
      if (!isCurrent()) return;
      if (isVersionConflict(e)) {
        setConflict(true);
        comparison.refresh();
      } else {
        setError(permissionMessageOf(e));
      }
    } finally {
      if (isCurrent()) setBusy(false);
    }
  }

  function deletePage(): void {
    if (id === null || formMismatch || pageId === null || version === null) return;
    const isCurrent = beginRequest();
    const current = readForm();
    modal.confirm({
      title: `移入页面回收站「${current.title || current.slug}」？`,
      content: `地址 /${baselineRef.current.slug} 会立即失效。可从页面回收站恢复为草稿；未保存的修改会丢失。`,
      okButtonProps: { danger: true },
      onOk: async () => {
        if (!isCurrent()) return;
        setBusy(true);
        setError(null);
        setNotice(null);
        setDeleteConflict(false);
        try {
          await pagesApi.trashPage(id, version);
          invalidateRelated();
          if (!isCurrent()) return;
          navigate(paths.pages, { replace: true });
        } catch (cause) {
          if (!isCurrent()) return;
          if (isVersionConflict(cause)) {
            setError("页面已被修改。请重新加载并核对最新内容后再决定是否删除。");
            setDeleteConflict(true);
          } else {
            setError(permissionMessageOf(cause));
          }
        } finally {
          if (isCurrent()) setBusy(false);
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
    { id, loadedId: loadedIdRef.current },
  );
  const canPublish = me?.permissions.includes("page.publish") ?? false;
  const canUnpublish = me?.permissions.includes("page.unpublish") ?? false;
  const canArchive = me?.permissions.includes("page.archive") ?? false;
  const canDelete = me?.permissions.includes("page.delete") ?? false;

  return {
    formApi,
    view,
    setView,
    version,
    pageStatus,
    publishedAt,
    loading,
    busy,
    notice,
    error,
    conflict,
    comparison,
    deleteConflict,
    mediaOpen,
    setMediaOpen,
    attachContentRef,
    pageId,
    formMismatch,
    unloaded,
    localDraft,
    save,
    reloadFromServer,
    overwriteWithLatest,
    changeStatus,
    deletePage,
    published,
    insertion,
    canReadMedia,
    canUploadMedia,
    canPublish,
    canUnpublish,
    canArchive,
    canDelete,
    savedSlug: baselineRef.current.slug,
  };
}
