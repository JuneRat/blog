import type { PostDetail, Visibility } from "../../types";

export interface FormState {
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

export const EMPTY_FORM: FormState = {
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

export function toForm(post: PostDetail): FormState {
  return {
    slug: post.slug,
    title: post.title,
    excerpt: post.excerpt ?? "",
    content: post.content,
    visibility: post.visibility,
    tagIds: [...post.tag_ids],
    categoryId: post.category_id,
    seriesIds: post.series.map((s) => s.series_id),
    seriesPositions: Object.fromEntries(
      post.series.map((s) => [s.series_id, String(s.position)]),
    ),
    coverMediaId: post.cover_media_id,
  };
}

/**
 * antd `Select` 的 `allowClear` 清空后给出 `undefined`，某些写法会给出 `""`；
 * `FormState` 用 `null` 表示未分类，空数组表示未选系列。这里统一收敛，
 * 否则 `pickServer`/`mergeServer` 会把 `undefined` 与 `null` 当成两种不同的值。
 */
export function normalizeForm(raw: FormState): FormState {
  const categoryId = raw.categoryId;
  const seriesIds = raw.seriesIds ?? [];
  const coverMediaId = raw.coverMediaId;
  return {
    ...raw,
    categoryId:
      typeof categoryId === "string" && categoryId.length > 0
        ? categoryId
        : null,
    seriesIds,
    seriesPositions: Object.fromEntries(
      seriesIds.map((id) => [id, String(raw.seriesPositions?.[id] ?? "0")]),
    ),
    // 封面选择器只上报 id 或 null；这里同样收敛 `""`/`undefined`，保持单值语义。
    coverMediaId:
      typeof coverMediaId === "string" && coverMediaId.length > 0
        ? coverMediaId
        : null,
  };
}

/** 集合相等（标签选择顺序无关）。 */
function sameTags(left: string[], right: string[]): boolean {
  if (left.length !== right.length) return false;
  const set = new Set(left);
  return right.every((id) => set.has(id));
}

export function formEquals(left: FormState, right: FormState): boolean {
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
  return (
    sameTags(left.seriesIds, right.seriesIds) &&
    left.seriesIds.every(
      (id) =>
        left.seriesPositions[id]?.trim() === right.seriesPositions[id]?.trim(),
    )
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
export function mergeServer(
  current: FormState,
  sent: FormState,
  server: FormState,
): FormState {
  // 标签是集合字段：用户没动过勾选才接受服务器值，动过则保留本地选择。
  const tags = sameTags(current.tagIds, sent.tagIds)
    ? server.tagIds
    : current.tagIds;
  const categoryId =
    current.categoryId === sent.categoryId
      ? server.categoryId
      : current.categoryId;
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

/** 权重允许 0，拒绝空白、负数、小数和超出数据库整数范围的值。 */
export function validSeries(current: FormState): boolean {
  return current.seriesIds.every((id) => {
    const raw = current.seriesPositions[id]?.trim() ?? "0";
    const n = Number(raw);
    return raw.length > 0 && Number.isInteger(n) && n >= 0 && n <= 2147483647;
  });
}

export function seriesPayload(current: FormState) {
  return current.seriesIds.map((id) => ({
    series_id: id,
    position: Number(current.seriesPositions[id] ?? "0"),
  }));
}
