import { useCallback, useEffect, useRef, useState } from "react";
import { App as AntdApp, Form } from "antd";
import { useQueryClient } from "@tanstack/react-query";
import { useEditorRequestGuard } from "../../useEditorRequestGuard";
import { useConflictSnapshot } from "../../useConflictSnapshot";
import { useLocalDraft } from "../../localDraft";
import { invalidateAfterWrite } from "../../queryEffects";
import {
  statusLabel,
  type ContentAction,
} from "../../components/ContentLifecycleControls";
import { ApiError } from "../../api/client";
import { postsApi } from "../../api/posts";
import type { EditPostInput } from "../../api/generated";
import { permissionMessageOf } from "../../apiError";
import { useAuth } from "../../auth";
import { navigate, paths } from "../../router";
import { useUnsavedGuard } from "../../unsaved";
import { useImageInsertion } from "../../components/useImageInsertion";
import type { MarkdownEditorHandle } from "../../components/editorHandle";
import type { PostDetail } from "../../types";
import {
  EMPTY_FORM,
  normalizeForm,
  toForm,
  formEquals,
  mergeServer,
  validSeries,
  seriesPayload,
  type FormState,
} from "./form";

function isVersionConflict(error: unknown): boolean {
  if (!(error instanceof ApiError) || error.status !== 409) return false;
  // code 缺失时按旧契约（409 即版本冲突）保守处理。
  return error.code === null || error.code === "version_conflict";
}

const watchForm = (values: FormState) =>
  normalizeForm({ ...EMPTY_FORM, ...values });

/** Server baseline and request lifecycle; editable values live only in Form store. */
export function usePostEditor(id: string | null) {
  const beginRequest = useEditorRequestGuard(id);
  const { me } = useAuth();
  const { modal } = AntdApp.useApp();
  const [formApi] = Form.useForm<FormState>();
  // useWatch batches notifications; reads during other renders must still see the current store.
  Form.useWatch(watchForm, { form: formApi, preserve: true });
  const view = normalizeForm({
    ...EMPTY_FORM,
    ...formApi.getFieldsValue(true),
  });
  const [version, setVersion] = useState<number | null>(null);
  const [postStatus, setPostStatus] = useState<string>("draft");
  const [publishedAt, setPublishedAt] = useState<string | null>(null);
  const [loading, setLoading] = useState(id !== null);
  const [busy, setBusy] = useState(false);
  const [commentBusy, setCommentBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);
  const comparison = useConflictSnapshot({
    active: conflict,
    id,
    load: postsApi.getPost,
  });
  const localSavedRef = useRef<
    (id: string, version: number, merged: FormState, dirty: boolean) => void
  >(() => {});
  /** 编辑器插图弹窗是否打开。 */
  const [mediaOpen, setMediaOpen] = useState(false);
  /** 当前编辑模式的插图接口，保存源码或 IR 选区。 */
  const contentRef = useRef<MarkdownEditorHandle | null>(null);
  /** 最近一次与服务器同步的表单内容，用于判断是否有未保存编辑。 */
  const baselineRef = useRef<FormState>(EMPTY_FORM);
  /** 创建成功后切到新 ID 时保留已合并的输入，避免重复加载覆盖。 */
  const loadedIdRef = useRef<string | null>(null);
  /**
   * 当前表单内容所属的 ID（`applyServer` 写入）。
   *
   * 编辑屏后退或切换到另一篇文章时组件会被复用（App 不按 ID 加 key）：
   * 若目标文章加载失败，表单里留着的仍是**上一篇**的正文与版本号。
   * 用「表单所属 ID」而不是「version 是否为空」判断，才能覆盖
   * A(已加载) → B(加载失败) 这条路径，避免把 A 的内容连 A 的 expected_version
   * 写到 B 的 ID 上（版本恰好相同就是静默覆盖）。
   * 与 `PageEditScreen` 的 `pageId`/`formMismatch`/`unloaded` 同源。
   */
  const [formId, setFormId] = useState<string | null>(null);

  const attachContentRef = useCallback((node: MarkdownEditorHandle | null): void => {
    contentRef.current = node;
  }, []);

  /** 读当前值。字段尚未注册（例如「系列内序号」）时用 EMPTY_FORM 补齐成完整的 FormState。 */
  const readForm = useCallback(
    (): FormState =>
      normalizeForm({ ...EMPTY_FORM, ...formApi.getFieldsValue(true) }),
    [formApi],
  );

  /** Programmatic changes update Form store; useWatch derives the rendered values. */
  const writeForm = useCallback(
    (next: FormState): void => {
      formApi.setFieldsValue(next);
    },
    [formApi],
  );

  /**
   * 用服务器响应同步表单，返回同步后是否仍有未保存改动。
   *
   * 传入 `sent`（发出请求时的表单快照）时逐字段合并：请求飞行期间的新输入被保留。
   * 不传表示显式丢弃本地改动（初次加载、用户点「重新加载」）。
   */
  const applyServer = useCallback(
    (post: PostDetail, sent?: FormState): boolean => {
      const server = toForm(post);
      const merged =
        sent === undefined ? server : mergeServer(readForm(), sent, server);
      baselineRef.current = server;
      loadedIdRef.current = post.id;
      writeForm(merged);
      setFormId(post.id);
      setVersion(post.version);
      setPostStatus(post.status);
      setPublishedAt(post.published_at);
      setConflict(false);
      const stillDirty = !formEquals(merged, server);
      if (sent !== undefined)
        localSavedRef.current(post.id, post.version, merged, stillDirty);
      return stillDirty;
    },
    [readForm, writeForm],
  );

  /**
   * 发布/撤回只改状态、不改正文：只同步状态类字段，表单与脏标记原样保留。
   * 否则用「保存时的旧正文」回填会覆盖用户在发布请求飞行期间的新输入。
   */
  const applyStatus = useCallback((post: PostDetail): void => {
    setVersion(post.version);
    setPostStatus(post.status);
    setPublishedAt(post.published_at);
    setConflict(false);
  }, []);

  /** 此刻是否仍有未保存改动（读 form store，可用于 await 之后）。 */
  function hasUnsaved(): boolean {
    return !formEquals(readForm(), baselineRef.current);
  }

  /** 表单内容与当前地址不一致：见 `formId` 的说明。 */
  const formMismatch = id !== null && formId !== id;
  /** 未加载成功（非加载中但表单仍不属于当前地址）：显示告警与重试入口。 */
  const unloaded = formMismatch && !loading;
  /** 渲染镜像与最近一次服务器同步值的差异；用于离开确认（见 src/unsaved.tsx）。 */
  const dirty = !formEquals(view, baselineRef.current);
  useUnsavedGuard(
    () => !formMismatch && !loading && hasUnsaved(),
    "文章有未保存的修改，离开会丢失。",
  );
  const localDraft = useLocalDraft({
    owner: me?.user_id,
    kind: "post",
    id,
    ready: !loading && !formMismatch && (id !== null || formId === null),
    disabled: busy || commentBusy,
    dirty,
    value: view,
    readValue: readForm,
    readDirty: hasUnsaved,
    template: EMPTY_FORM,
    baselineVersion: version,
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
  /** 每次提交后同步使列表、目录统计、媒体引用和评论关联信息过期。 */
  const invalidateRelated = useCallback((): void => {
    void invalidateAfterWrite(queryClient, "post");
  }, [queryClient]);

  useEffect(() => {
    setBusy(false);
    if (id === null || id !== loadedIdRef.current) setMediaOpen(false);
    setNotice(null);
    setError(null);
    setConflict(false);
    if (id === null) {
      // 编辑页后退到新建页时组件会复用；清空全部编辑状态。
      // 创建成功的 null → ID 跳转仍由 loadedIdRef 保留已合并的新输入。
      baselineRef.current = EMPTY_FORM;
      loadedIdRef.current = null;
      writeForm(EMPTY_FORM);
      setFormId(null);
      setVersion(null);
      setPostStatus("draft");
      setPublishedAt(null);
      setBusy(false);
      setNotice(null);
      setError(null);
      setConflict(false);
      setLoading(false);
      return;
    }
    // 刚创建并同步过这篇文章：切到新 ID 时不要重载覆盖请求期间的新输入。
    if (id === loadedIdRef.current) {
      setLoading(false);
      return;
    }
    let cancelled = false;
    void (async () => {
      setLoading(true);
      try {
        const post = await postsApi.getPost(id);
        if (!cancelled) applyServer(post);
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

  /** 只在 slug 变化时提交 new_slug，避免把未改动值当作改名；读 store 取最新编辑。
   *  标签集合总是提交：后端按「整体替换」处理，未变化的集合是幂等重写。 */
  function editPayload(): EditPostInput {
    const current = readForm();
    const trimmed = current.slug.trim();
    return {
      new_slug:
        id !== null &&
        trimmed.length > 0 &&
        trimmed !== baselineRef.current.slug
          ? trimmed
          : undefined,
      title: current.title,
      excerpt: current.excerpt,
      content: current.content,
      visibility: current.visibility,
      tag_ids: current.tagIds,
      category_id: current.categoryId,
      // 封面始终显式提交：后端按绝对值处理（null = 移除，id = 设置），
      // 表单里没有「缺省不触碰」这一态，整表保存正好覆盖它。
      cover_media_id: current.coverMediaId,
      series: seriesPayload(current),
    };
  }

  async function save(): Promise<void> {
    const isCurrent = beginRequest();
    if (localDraft.blocksEditing) {
      setError("请先恢复或丢弃当前窗口的本机副本。");
      return;
    }
    // 表单不属于当前地址时绝不能写入：`expected_version` 会用上一篇的版本号
    // 打到另一个 ID 上，版本相同即静默覆盖。（加载途中同样适用，故用 formMismatch 而非 unloaded。）
    if (formMismatch) {
      setError("文章尚未成功加载，请先重新加载再保存，避免覆盖服务器内容。");
      return;
    }
    // 系列序号是提交前提：非法值直接阻止，绝不静默丢弃系列选择。
    if (!validSeries(readForm())) {
      setError("系列排序权重必须是非负整数。");
      return;
    }
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      if (id === null) {
        const sent = readForm();
        const created = await postsApi.createPost({
          slug: sent.slug.trim().length > 0 ? sent.slug.trim() : undefined,
          title: sent.title,
          excerpt:
            sent.excerpt.trim().length > 0 ? sent.excerpt.trim() : undefined,
          content: sent.content,
          visibility: sent.visibility,
          tag_ids: sent.tagIds,
          category_id: sent.categoryId ?? undefined,
          // 新建时只在确实选了封面才发送；未选 = 不设封面。
          cover_media_id: sent.coverMediaId ?? undefined,
          series: seriesPayload(sent) ?? undefined,
        });
        // 先本地同步（含创建期间的新输入），再更新地址；效果钩子会跳过重载。
        invalidateRelated();
        if (!isCurrent()) return;
        applyServer(created, sent);
        navigate(paths.editPost(created.id));
        return;
      }
      const sent = readForm();
      const saved = await postsApi.updatePost(id, {
        ...editPayload(),
        expected_version: version ?? undefined,
      });
      // 合并而不是整体覆盖：请求飞行期间的新输入必须保留。
      invalidateRelated();
      if (!isCurrent()) return;
      const stillDirty = applyServer(saved, sent);

      setNotice(stillDirty ? "已保存；等待期间的新改动尚未保存。" : "已保存。");
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
      const result = await postsApi.getPost(id);
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
      content:
        "请先核对差异。若服务器再次变化，本次覆盖会被拒绝，本地输入会保留。",
      okButtonProps: { danger: true },
      onOk: async () => {
        if (!isCurrent()) return;
        setError(null);
        setBusy(true);
        try {
          // 确认期间继续输入时，提交当前输入；版本始终绑定已展示的快照。
          const sent = readForm();
          const saved = await postsApi.updatePost(id, {
            ...editPayload(),
            expected_version: latest.version,
          });
          invalidateRelated();
          if (!isCurrent()) return;
          const stillDirty = applyServer(saved, sent);

          setNotice(
            stillDirty
              ? "已覆盖保存；等待期间的新改动尚未保存。"
              : "已用服务器最新版本覆盖保存。",
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

  /**
   * 发布/撤回。这两个动作只改状态、不写正文：如果先把服务器返回的旧正文回填表单，
   * 本地未保存的编辑会被静默丢弃，线上发布的也仍是上一版内容。
   * 发布/预约先保存；撤回/归档只改服务器状态，本地编辑继续保留。
   */
  async function changeStatus(
    action: ContentAction,
    at?: string,
  ): Promise<void> {
    const isCurrent = beginRequest();
    // 表单不属于当前地址时不得改状态：没有可依据的版本号，等于盲写。
    if (id === null || formMismatch) return;
    // 撤回和归档不保存本地编辑，不能被未提交字段的校验阻止。
    if (
      (action === "publish" || action === "schedule") &&
      !validSeries(readForm())
    ) {
      setError("系列排序权重必须是非负整数。");
      return;
    }
    setError(null);
    setNotice(null);
    setBusy(true);
    const hadUnsavedEdits = hasUnsaved();
    const savesContent = action === "publish" || action === "schedule";
    try {
      let expected = version ?? undefined;
      if (savesContent && hadUnsavedEdits && postStatus !== "archived") {
        const sent = readForm();
        const saved = await postsApi.updatePost(id, {
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
      // 发布/撤回不改正文：只同步状态，保留（可能还在变化的）表单。
      const result =
        action === "publish"
          ? await postsApi.publishPost(id, expected)
          : action === "schedule"
            ? await postsApi.schedulePost(id, at!, expected)
            : action === "archive"
              ? await postsApi.archivePost(id, expected)
              : await postsApi.unpublishPost(id, expected);
      invalidateRelated();
      if (!isCurrent()) return;
      applyStatus(result);
      setNotice(
        `状态已更新为${statusLabel(result.status)}。${hasUnsaved() ? "还有未保存的改动。" : ""}`,
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

  const canUnpublish =
    me?.permissions.some(
      (key) => key === "post.unpublish" || key === "post.unpublish_any",
    ) ?? false;
  const canPublish =
    me?.permissions.some(
      (key) => key === "post.publish" || key === "post.publish_any",
    ) ?? false;
  const canReadMedia = me?.permissions.includes("media.read") ?? false;
  const canUploadMedia = me?.permissions.includes("media.upload") ?? false;
  /**
   * 图片插入：拖入/粘贴上传与面板插入共用同一路径，插入只改本地表单。
   * 已发布内容仍然要显式点「保存并更新线上」才生效。
   */
  const insertion = useImageInsertion(
    contentRef,
    () => readForm().content,
    commitContent,
    { id, loadedId: loadedIdRef.current },
  );

  function onCommentSaved(next: number, previous: number): void {
    if (loadedIdRef.current === id)
      setVersion((current) => (current === previous ? next : current));
  }
  return {
    formApi,
    view,
    version,
    postStatus,
    publishedAt,
    loading,
    busy,
    commentBusy,
    notice,
    error,
    conflict,
    comparison,
    mediaOpen,
    setMediaOpen,
    attachContentRef,
    savedSlug: baselineRef.current.slug,
    formMismatch,
    unloaded,
    localDraft,
    save,
    reloadFromServer,
    overwriteWithLatest,
    changeStatus,
    canPublish,
    canUnpublish,
    canReadMedia,
    canUploadMedia,
    insertion,
    setCommentBusy,
    onCommentSaved,
  };
}
