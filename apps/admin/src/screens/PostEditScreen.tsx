import { ContentLifecycleControls, statusLabel, type ContentAction } from "../components/ContentLifecycleControls";
import { CommentSwitch } from "../components/CommentSwitch";
import {
  Alert,
  App as AntdApp,
  Button,
  Checkbox,
  Flex,
  Form,
  Input,
  Select,
  Space,
  Tag,
  Typography,
} from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, api, categoryApi, seriesApi } from "../api";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import { navigate, paths } from "../router";
import { useLeaveConfirmation, useUnsavedGuard } from "../unsaved";
import { MediaInsertPanel } from "../components/MediaInsertPanel";
import { CoverPicker } from "../components/CoverPicker";
import { useImageInsertion } from "../components/useImageInsertion";
import type { EditPostInput } from "../api";
import type { PostDetail, Visibility } from "../types";

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
  /** 系列 id 集合（空数组 = 不属于任何系列）。 */
  seriesIds: string[];
  /** 每个已选系列内的排序权重（非负整数，可重复）。 */
  seriesPositions: Record<string, string>;
  /**
   * 封面媒体 id（null = 无封面）。
   *
   * 表单里只存一个可空值——「移除封面」就是把它设为 null；提交时再按后端
   * PATCH 的三态语义始终显式带上该字段（见 `editPayload`）。
   */
  coverMediaId: string | null;
}

const EMPTY_FORM: FormState = {
  slug: "",
  title: "",
  excerpt: "",
  content: "",
  visibility: "public",
  tagIds: [],
  categoryId: null,
  seriesIds: [],
  seriesPositions: {},
  coverMediaId: null,
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
    seriesIds: post.series.map((s) => s.series_id),
    seriesPositions: Object.fromEntries(post.series.map((s) => [s.series_id, String(s.position)])),
    coverMediaId: post.cover_media_id,
  };
}

/**
 * antd `Select` 的 `allowClear` 清空后给出 `undefined`，某些写法会给出 `""`；
 * `FormState` 用 `null` 表示未分类，空数组表示未选系列。这里统一收敛，
 * 否则 `pickServer`/`mergeServer` 会把 `undefined` 与 `null` 当成两种不同的值。
 */
function normalizeForm(raw: FormState): FormState {
  const categoryId = raw.categoryId;
  const seriesIds = raw.seriesIds ?? [];
  const coverMediaId = raw.coverMediaId;
  return {
    ...raw,
    categoryId: typeof categoryId === "string" && categoryId.length > 0 ? categoryId : null,
    seriesIds,
    seriesPositions: Object.fromEntries(
      seriesIds.map((id) => [id, String(raw.seriesPositions?.[id] ?? "0")]),
    ),
    // 封面选择器只上报 id 或 null；这里同样收敛 `""`/`undefined`，保持单值语义。
    coverMediaId:
      typeof coverMediaId === "string" && coverMediaId.length > 0 ? coverMediaId : null,
  };
}

/** 集合相等（标签选择顺序无关）。 */
function sameTags(left: string[], right: string[]): boolean {
  if (left.length !== right.length) return false;
  const set = new Set(left);
  return right.every((id) => set.has(id));
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
    sameSeries(left, right) &&
    left.coverMediaId === right.coverMediaId
  );
}

function sameSeries(left: FormState, right: FormState): boolean {
  return sameTags(left.seriesIds, right.seriesIds) && left.seriesIds.every(
    (id) => left.seriesPositions[id]?.trim() === right.seriesPositions[id]?.trim(),
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
  const seriesSource = sameSeries(current, sent) ? server : current;
  return {
    slug: pickServer("slug", current, sent, server),
    title: pickServer("title", current, sent, server),
    excerpt: pickServer("excerpt", current, sent, server),
    content: pickServer("content", current, sent, server),
    visibility: pickServer("visibility", current, sent, server),
    tagIds: tags,
    categoryId,
    seriesIds: seriesSource.seriesIds,
    seriesPositions: seriesSource.seriesPositions,
    // 封面与分类一样是单值字段：用户没在请求飞行期间改过才接受服务器值。
    coverMediaId: pickServer("coverMediaId", current, sent, server),
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

/** antd `Input.TextArea` 的 ref 不是 DOM 节点，取出里面的原生 textarea 供插入逻辑使用。 */
type TextAreaHandle = { resizableTextArea?: { textArea: HTMLTextAreaElement } };

/**
 * 编辑屏。`id === null` 表示新建。
 *
 * 冲突策略（docs/content-lifecycle.md §1）：写入携带 expected_version；
 * 409 时**保留客户端编辑并提示处理**，不自动覆盖：
 * - 「重新加载」拉取服务器最新内容并丢弃本地改动；
 * - 「仍然覆盖」二次确认后，用**服务器最新 version** 重新提交本地内容。
 *
 * 值的唯一来源是 antd Form 的 store；`view` 只是给渲染与脏判断用的镜像，
 * 由 `onValuesChange`（同步回调）与写入函数共同维护，不另立第二份数据。
 * 这样 `pickServer`/`mergeServer` 仍能读到「响应回来那一刻」的真实输入，
 * 保留「请求飞行期间的新输入不被服务器响应覆盖」的既有语义。
 */
export function PostEditScreen({ id }: { id: string | null }) {
  const { me } = useAuth();
  const { modal } = AntdApp.useApp();
  const [formApi] = Form.useForm<FormState>();
  const [view, setView] = useState<FormState>(EMPTY_FORM);
  const [version, setVersion] = useState<number | null>(null);
  const [postStatus, setPostStatus] = useState<string>("draft");
  const [publishedAt, setPublishedAt] = useState<string | null>(null);
  const [loading, setLoading] = useState(id !== null);
  const [busy, setBusy] = useState(false);
  const [commentBusy, setCommentBusy] = useState(false);
  /**
   * 三个目录（标签/分类/系列）走 React Query：与标签、分类、系列三个管理屏
   * 共用同一份缓存，编辑器之间也不再各拉一遍。
   *
   * 目录加载失败**不阻塞正文编辑**——只是暂时无法勾选，所以这里不进入 loading 分支，
   * 只在选择区上方显示一条错误。
   */
  const tagsQuery = useQuery({ queryKey: queryKeys.tags(), queryFn: () => api.listTags() });
  const categoriesQuery = useQuery({
    queryKey: queryKeys.categories(),
    queryFn: () => categoryApi.list(),
  });
  const seriesQuery = useQuery({
    queryKey: queryKeys.series(),
    queryFn: () => seriesApi.list(),
  });
  const catalog = tagsQuery.data ?? null;
  const categoryCatalog = categoriesQuery.data ?? null;
  const seriesCatalog = seriesQuery.data ?? null;
  const catalogFailure = tagsQuery.error ?? categoriesQuery.error ?? seriesQuery.error;
  const catalogError = catalogFailure === null ? null : permissionMessageOf(catalogFailure);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);
  /** 图片面板是否展开（编辑器内插入图片）。 */
  const [mediaOpen, setMediaOpen] = useState(false);
  /** 正文输入框：插入位置取自它的真实选区。 */
  const contentRef = useRef<HTMLTextAreaElement | null>(null);
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

  const attachContentRef = useCallback((node: TextAreaHandle | null): void => {
    contentRef.current = node?.resizableTextArea?.textArea ?? null;
  }, []);

  /** 读当前值。字段尚未注册（例如「系列内序号」）时用 EMPTY_FORM 补齐成完整的 FormState。 */
  const readForm = useCallback(
    (): FormState => normalizeForm({ ...EMPTY_FORM, ...formApi.getFieldsValue(true) }),
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

  /**
   * 用服务器响应同步表单，返回同步后是否仍有未保存改动。
   *
   * 传入 `sent`（发出请求时的表单快照）时逐字段合并：请求飞行期间的新输入被保留。
   * 不传表示显式丢弃本地改动（初次加载、用户点「重新加载」）。
   */
  const applyServer = useCallback(
    (post: PostDetail, sent?: FormState): boolean => {
      const server = toForm(post);
      const merged = sent === undefined ? server : mergeServer(readForm(), sent, server);
      baselineRef.current = server;
      loadedIdRef.current = post.id;
      writeForm(merged);
      setFormId(post.id);
      setVersion(post.version);
      setPostStatus(post.status);
      setPublishedAt(post.published_at);
      setConflict(false);
      return !formEquals(merged, server);
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
  useUnsavedGuard(dirty, "文章有未保存的修改，离开会丢失。");
  const queryClient = useQueryClient();
  /**
   * 写成功后让文章列表失效。
   *
   * 列表查询是 30s 新鲜度、且关闭了窗口聚焦重取；不显式失效的话，
   * 「改完标题/改名 → 返回列表」会命中旧缓存，看到的是保存前的标题与 slug。
   * 失效只打标记，真正取数发生在列表下次挂载时（此时它多半不在挂载状态）。
   */
  const invalidateList = useCallback((): void => {
    void queryClient.invalidateQueries({ queryKey: queryKeys.posts() });
  }, [queryClient]);

  const confirmLeave = useLeaveConfirmation();


  useEffect(() => {
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
        const post = await api.getPost(id);
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
      new_slug: id !== null && trimmed.length > 0 && trimmed !== baselineRef.current.slug ? trimmed : undefined,
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

  /** 权重允许 0，拒绝空白、负数、小数和超出数据库整数范围的值。 */
  function validSeries(current: FormState): boolean {
    return current.seriesIds.every((id) => {
      const raw = current.seriesPositions[id]?.trim() ?? "0";
      const n = Number(raw);
      return raw.length > 0 && Number.isInteger(n) && n >= 0 && n <= 2147483647;
    });
  }

  function seriesPayload(current: FormState) {
    return current.seriesIds.map((id) => ({
      series_id: id,
      position: Number(current.seriesPositions[id] ?? "0"),
    }));
  }

  async function save(): Promise<void> {
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
        const created = await api.createPost({
          slug: sent.slug.trim().length > 0 ? sent.slug.trim() : undefined,
          title: sent.title,
          excerpt: sent.excerpt.trim().length > 0 ? sent.excerpt.trim() : undefined,
          content: sent.content,
          visibility: sent.visibility,
          tag_ids: sent.tagIds,
          category_id: sent.categoryId ?? undefined,
          // 新建时只在确实选了封面才发送；未选 = 不设封面。
          cover_media_id: sent.coverMediaId ?? undefined,
          series: seriesPayload(sent) ?? undefined,
        });
        // 先本地同步（含创建期间的新输入），再更新地址；效果钩子会跳过重载。
        applyServer(created, sent);
        invalidateList();
        navigate(paths.editPost(created.id));
        return;
      }
      const sent = readForm();
      const saved = await api.updatePost(id, {
        ...editPayload(),
        expected_version: version ?? undefined,
      });
      // 合并而不是整体覆盖：请求飞行期间的新输入必须保留。
      const stillDirty = applyServer(saved, sent);
      invalidateList();

      setNotice(
        stillDirty ? "已保存；等待期间的新改动尚未保存。" : "已保存（已发布内容直接更新线上）。",
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
    if (id === null) return;
    setError(null);
    setBusy(true);
    try {
      applyServer(await api.getPost(id));
      setNotice("已重新加载服务器最新内容。");
    } catch (e) {
      setError(permissionMessageOf(e));
    } finally {
      setBusy(false);
    }
  }

  /** 冲突动作二：二次确认后用服务器最新 version 覆盖本地内容。 */
  function overwriteWithLatest(): void {
    if (id === null) return;
    modal.confirm({
      title: "用当前编辑内容覆盖服务器上的最新版本？",
      content: "将用你当前的编辑内容覆盖服务器上的最新版本，确定继续？",
      okButtonProps: { danger: true },
      onOk: async () => {
        setError(null);
        setBusy(true);
        try {
          const latest = await api.getPost(id);
          // 等待 getPost 期间的新输入也要一起提交，不能在覆盖时丢掉。
          const sent = readForm();
          const saved = await api.updatePost(id, {
            ...editPayload(),
            expected_version: latest.version,
          });
          const stillDirty = applyServer(saved, sent);
          invalidateList();

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

  /**
   * 发布/撤回。这两个动作只改状态、不写正文：如果先把服务器返回的旧正文回填表单，
   * 本地未保存的编辑会被静默丢弃，线上发布的也仍是上一版内容。
   * 因此有未保存改动时先保存，再用保存得到的版本发布/撤回。
   */
  async function changeStatus(action: ContentAction, at?: string): Promise<void> {
    // 表单不属于当前地址时不得改状态：没有可依据的版本号，等于盲写。
    if (id === null || formMismatch) return;
    // 保存未存编辑走同一前提校验。
    if (!validSeries(readForm())) {
      setError("系列排序权重必须是非负整数。");
      return;
    }
    setError(null);
    setNotice(null);
    setBusy(true);
    const hadUnsavedEdits = hasUnsaved();
    try {
      let expected = version ?? undefined;
      if (hadUnsavedEdits && postStatus !== "archived") {
        const sent = readForm();
        const saved = await api.updatePost(id, {
          ...editPayload(),
          expected_version: expected,
        });
        expected = saved.version;
        applyServer(saved, sent);
        /**
         * 先行保存已经改动了列表数据（标题/slug/版本）：**立刻失效**，不能等到
         * 状态切换成功——那一步失败时返回列表看到的还是保存前的数据。
         * 下面那次失效仍要保留：状态列（已发布/草稿）只有状态切换成功才变。
         */
        invalidateList();
      }
      // 发布/撤回不改正文：只同步状态，保留（可能还在变化的）表单。
      const result = action === "publish"
        ? await api.publishPost(id, expected)
        : action === "schedule" ? await api.schedulePost(id, at!, expected)
        : action === "archive" ? await api.archivePost(id, expected)
        : await api.unpublishPost(id, expected);
      applyStatus(result);
      invalidateList();
      setNotice(`状态已更新为${statusLabel(result.status)}。${hasUnsaved() ? "还有未保存的改动。" : ""}`);
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

  const canUnpublish = me?.permissions.some((key) => key === "post.unpublish" || key === "post.unpublish_any") ?? false;
  const canPublish = me?.permissions.some((key) => key === "post.publish" || key === "post.publish_any") ?? false;
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
  );

  if (loading) {
    return <Typography.Text type="secondary">正在加载…</Typography.Text>;
  }

  return (
    <>
      <Flex justify="space-between" align="center" wrap gap={12} style={{ marginBottom: 16 }}>
        <Flex align="center" gap={8}>
          <Typography.Title level={3} style={{ margin: 0 }}>
            {id === null ? "新建草稿" : view.slug}
          </Typography.Title>
          <Tag color={postStatus === "published" ? "green" : undefined}>
            {statusLabel(postStatus)}
          </Tag>
          {version !== null && <Typography.Text type="secondary">v{version}</Typography.Text>}
        </Flex>
        {postStatus === "published" && id !== null && (
          <Typography.Link
            href={`/posts/${encodeURIComponent(baselineRef.current.slug)}`}
            target="_blank"
            rel="noreferrer"
          >
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
              <Button disabled={busy || commentBusy} onClick={() => void reloadFromServer()}>
                重新加载（丢弃本地改动）
              </Button>
              <Button danger disabled={busy || commentBusy} onClick={overwriteWithLatest}>
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
      {unloaded && (
        <Alert
          type="warning"
          showIcon
          title="文章未能加载。"
          description="可能已被删除或暂时不可达。已禁用保存与发布，避免把上一次打开的内容写到这个地址。"
          style={{ marginBottom: 16 }}
          action={
            <Button disabled={busy || commentBusy} onClick={() => void reloadFromServer()}>
              重新加载
            </Button>
          }
        />
      )}

      {id !== null && <CommentSwitch key={id} post={id} expectedVersion={version} disabled={busy || formMismatch}
        onBusy={setCommentBusy} onSaved={(next, previous) => {
          if (loadedIdRef.current === id) setVersion(current => current === previous ? next : current);
          invalidateList();
        }} />}
      <Form
        form={formApi}
        disabled={postStatus === "archived" || formMismatch}
        layout="vertical"
        initialValues={EMPTY_FORM}
        onValuesChange={(_changed, all) => setView(normalizeForm({ ...EMPTY_FORM, ...all }))}
        onFinish={() => { if (!commentBusy) void save(); }}
      >
        <Form.Item label="slug" name="slug">
          <Input placeholder="留空则自动生成（预约或发布后锁定）" />
        </Form.Item>
        <Form.Item label="标题" name="title">
          <Input />
        </Form.Item>
        <Form.Item label="摘要" name="excerpt">
          <Input />
        </Form.Item>
        {/*
          封面：隐藏的 Form.Item 负责把 coverMediaId 注册进表单 store，
          readForm/mergeServer/pickServer 才能像 categoryId 一样按字段读写；
          真正的控件是下面的 CoverPicker，值由 view 驱动、变化经 writeForm 回写。
          这样选择器不必依赖 Form.Item 的 value/onChange 注入，独立使用时也是同一套契约。
        */}
        <Form.Item name="coverMediaId" hidden>
          <Input />
        </Form.Item>
        <CoverPicker
          value={view.coverMediaId}
          onChange={(id) => writeForm({ ...readForm(), coverMediaId: id })}
          canReadMedia={canReadMedia}
          canUploadMedia={canUploadMedia}
        />
        <Form.Item label="可见性" name="visibility">
          <Select
            options={[
              { value: "public", label: "公开" },
              { value: "private", label: "私有" },
            ]}
          />
        </Form.Item>
        <Form.Item label="分类" name="categoryId">
          <Select
            allowClear
            placeholder="（未分类）"
            options={(categoryCatalog ?? []).map((category) => ({
              value: category.id,
              label: category.name,
            }))}
          />
        </Form.Item>
        <Form.Item label="系列" name="seriesIds">
          <Select
            mode="multiple"
            allowClear
            placeholder="可加入多个系列"
            options={(seriesCatalog ?? []).map((series) => ({ value: series.id, label: series.name }))}
          />
        </Form.Item>
        {view.seriesIds.map((seriesId) => (
          <Form.Item
            key={seriesId}
            label={`${seriesCatalog?.find((s) => s.id === seriesId)?.name ?? seriesId} · 排序权重`}
            name={["seriesPositions", seriesId]}
            initialValue="0"
          >
            <Input inputMode="numeric" placeholder="0 起；数值越小越靠前，允许重复" />
          </Form.Item>
        ))}
        <Form.Item label="标签" name="tagIds">
          <Checkbox.Group
            options={(catalog ?? []).map((tag) => ({ label: tag.name, value: tag.id }))}
          />
        </Form.Item>
        {catalogError !== null && (
          <Alert type="error" showIcon title={catalogError} style={{ marginBottom: 16 }} />
        )}
        {catalog === null && catalogError === null && (
          <Typography.Text type="secondary" style={{ display: "block", marginBottom: 16 }}>
            正在加载标签目录…
          </Typography.Text>
        )}
        {catalog !== null && catalog.length === 0 && (
          <Typography.Text type="secondary" style={{ display: "block", marginBottom: 16 }}>
            还没有可用标签；先在
            <Typography.Link
              href={paths.tags}
              onClick={(event) => {
                event.preventDefault();
                // 屏内离页入口走同一确认口径，否则保护只覆盖侧栏菜单。
                confirmLeave(() => navigate(paths.tags));
              }}
            >
              标签目录
            </Typography.Link>
            创建。
          </Typography.Text>
        )}
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
          {/*
            用文案切换而不是 Button 的 loading 属性表示进行中。
            antd 的 loading 图标在动画结束后仍会留在 DOM 里（jsdom 里动画不结束，
            查询更明显），其 `role="img" aria-label="loading"` 会污染按钮的无障碍名，
            让按名字定位变脆、读屏也会念出多余的 "loading"。
          */}
          <Button type="primary" htmlType="submit" disabled={busy || commentBusy || formMismatch || postStatus === "archived"}>
            {busy ? "处理中…" : "保存并更新线上"}
          </Button>
          {id !== null && <ContentLifecycleControls status={postStatus} publishedAt={publishedAt} disabled={busy || commentBusy || formMismatch}
            canPublish={canPublish} canUnpublish={canUnpublish} canArchive={canUnpublish} onAction={changeStatus} />}

        </Space>
      </Form>
    </>
  );
}
