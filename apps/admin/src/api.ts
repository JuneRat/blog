import type {
  AdminUser,
  CreatedUser,
  Me,
  MediaAsset,
  MediaPage,
  MediaUsageView,
  PageDetail,
  PageSummary,
  PasswordLoginResult,
  PostDetail,
  PostSummary,
  Profile,
  ProviderSummary,
  RoleSummary,
  SeriesSummary,
  CategorySummary,
  SeriesMemberRow,
  SiteSettings,
  ThemeSettings,
  TagSummary,
  Visibility,
} from "./types";

/**
 * 后端统一错误契约 `{error, code}`；status 供调用方分支（401/403/409）。
 *
 * `code` 是业务码：同一个 409 既可能是版本冲突（version_conflict，可重试覆盖），
 * 也可能是 slug 被占用（conflict，重试无用），只看状态码无法区分。
 * 旧响应可能没有 code，此时为 null。
 */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string | null;
  /** 服务端 `x-request-id`；从响应头读取，非 JSON 错误（如 500 HTML）同样可用。 */
  readonly requestId: string | null;

  constructor(
    status: number,
    message: string,
    code: string | null = null,
    requestId: string | null = null,
  ) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.requestId = requestId;
  }
}

/**
 * 在用户可见文案后附上请求编号，便于报障时与服务端日志对齐。
 * 编号为空（例如开发期直接调用未走中间件的路由）时原样返回。
 */
export function withRequestId(message: string, requestId: string | null): string {
  return requestId === null || requestId.length === 0
    ? message
    : `${message}（错误编号 ${requestId}）`;
}

/**
 * CSRF token 只保存在内存：登录后从 `/me` 取一次，刷新页面重新取。
 * 绝不写入 localStorage/sessionStorage（docs/identity-and-admin.md §4/§5）。
 */
let csrfToken: string | null = null;
let unauthorizedHandler: (() => void) | null = null;

export function setCsrfToken(token: string | null): void {
  csrfToken = token;
}

/** 401 处理：清空内存 token，由应用层跳登录。 */
export function setUnauthorizedHandler(handler: (() => void) | null): void {
  unauthorizedHandler = handler;
}

function errorMessage(data: unknown, fallback: string): string {
  if (data !== null && typeof data === "object" && "error" in data) {
    const value = (data as { error?: unknown }).error;
    if (typeof value === "string" && value.length > 0) return value;
  }
  return fallback;
}

/** 读取错误响应里的业务码；缺失或非字符串时为 null。 */
function errorCode(data: unknown): string | null {
  if (data !== null && typeof data === "object" && "code" in data) {
    const value = (data as { code?: unknown }).code;
    if (typeof value === "string" && value.length > 0) return value;
  }
  return null;
}

/**
 * 统一请求执行：认证凭据、错误契约与 401 处理都在这里，调用方只负责
 * 「body 类型」这一唯一差异（JSON 文本 vs 二进制图片）。
 */
async function perform<T>(
  path: string,
  init: RequestInit,
  jsonBody: boolean,
): Promise<T> {
  const method = (init.method ?? "GET").toUpperCase();
  const headers = new Headers(init.headers);
  if (jsonBody) headers.set("Content-Type", "application/json");
  if (method !== "GET" && method !== "HEAD" && csrfToken !== null) {
    headers.set("X-CSRF-Token", csrfToken);
  }

  const response = await fetch(path, { ...init, headers, credentials: "same-origin" });
  const text = await response.text();
  let data: unknown = null;
  if (text.length > 0) {
    try {
      data = JSON.parse(text);
    } catch {
      data = null;
    }
  }

  if (!response.ok) {
    if (response.status === 401) {
      csrfToken = null;
      unauthorizedHandler?.();
    }
    throw new ApiError(
      response.status,
      errorMessage(data, response.statusText),
      errorCode(data),
      response.headers.get("x-request-id"),
    );
  }
  return data as T;
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  return perform<T>(path, init, init.body !== undefined);
}

/** 二进制上传：不能把 Content-Type 写死为 JSON，其余契约与 `request` 一致。 */
async function requestBinary<T>(path: string, init: RequestInit): Promise<T> {
  return perform<T>(path, init, false);
}

export interface CreatePostInput {
  slug?: string;
  title: string;
  excerpt?: string;
  content: string;
  visibility: Visibility;
  /** 初始标签 id 集合；重复由后端去重。 */
  tag_ids?: string[];
  /** 初始分类 id。 */
  category_id?: string;
  /** 初始封面媒体 id；缺省/`null` = 不设封面。 */
  cover_media_id?: string | null;
  /** 初始系列与序号。 */
  series?: { id: string; order: number };
}

export interface EditPostInput {
  new_slug?: string;
  title?: string;
  excerpt?: string;
  content?: string;
  visibility?: Visibility;
  /** 存在即整体替换标签集合（[] = 清空）；缺省不触碰。 */
  tag_ids?: string[];
  /** null = 清空分类；id = 设置；缺省不触碰。 */
  category_id?: string | null;
  /**
   * 封面的**三态**：缺省 = 不触碰；`null` = 移除封面；id = 设置封面。
   *
   * 与 `category_id` 不同，编辑器的保存一律显式带上当前值（`null` 表示确实要移除），
   * 这样「移除封面」才能与「不改封面」区分开。
   */
  cover_media_id?: string | null;
  /** null = 退出系列；对象 = 设置系列与序号；缺省不触碰。 */
  series?: { id: string; order: number } | null;
  expected_version?: number;
}

export interface CreatePageInput {
  slug?: string;
  title: string;
  content: string;
  visibility: Visibility;
}

export interface EditPageInput {
  new_slug?: string;
  title?: string;
  content?: string;
  visibility?: Visibility;
  expected_version?: number;
}

export interface PasswordLoginInput {
  username: string;
  password: string;
  /** 仅本站相对路径；缺省由后端使用 /admin/。 */
  next?: string;
}

export interface CreateUserInput {
  username: string;
  email?: string;
  display_name?: string;
}

export interface CreateTagInput {
  name: string;
  slug: string;
}

export interface RenameTagInput {
  name: string;
  expected_version?: number;
}

export const api = {
  me: (): Promise<Me> => request<Me>("/api/admin/v1/me"),

  /**
   * 自助设置/清除头像（本人即可，无需额外权限）。
   * `null` = 清除；成功后返回最新资料，调用方应刷新 `/me` 让头部同步。
   * 不递增 users.version，因此不会让当前会话失效。
   */
  setOwnAvatar: (avatarMediaId: string | null): Promise<Profile> =>
    request<Profile>("/api/admin/v1/me/avatar", {
      method: "PUT",
      body: JSON.stringify({ avatar_media_id: avatarMediaId }),
    }),

  /** 自助改密：成功后服务端轮换会话并回新 csrf_token（调用方负责更新内存 token）。 */
  changeOwnPassword: (input: { current_password?: string; new_password: string }): Promise<{ user_id: string; csrf_token: string }> =>
    request<{ user_id: string; csrf_token: string }>("/api/admin/v1/me/password", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  providers: (): Promise<ProviderSummary[]> => request<ProviderSummary[]>("/auth/providers"),

  /**
   * 本地密码登录：成功后服务端下发会话 cookie，调用方随后刷新 `/me`。
   * 失败（用户名不存在或密码错误）统一 401 `invalid_credentials`；
   * 连续失败会被限流（429 `rate_limited`，`Retry-After` 给出等待秒数）。
   */
  loginWithPassword: (input: PasswordLoginInput): Promise<PasswordLoginResult> =>
    request<PasswordLoginResult>("/auth/login/password", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  listPosts: (author?: string): Promise<PostSummary[]> =>
    request<PostSummary[]>(
      `/api/admin/v1/posts${author ? `?author=${encodeURIComponent(author)}` : ""}`,
    ),

  listTrash: (page = 1, author?: string): Promise<{ items: PostSummary[]; total: number; page: number; per_page: number }> =>
    request(`/api/admin/v1/post-trash?page=${page}${author ? `&author=${encodeURIComponent(author)}` : ""}`),

  trashPost: (slug: string, expectedVersion: number): Promise<PostDetail> =>
    request(`/api/admin/v1/posts/${encodeURIComponent(slug)}/trash`, { method: "POST", body: JSON.stringify({ expected_version: expectedVersion }) }),

  restorePost: (slug: string, expectedVersion: number): Promise<PostDetail> =>
    request(`/api/admin/v1/posts/${encodeURIComponent(slug)}/restore`, { method: "POST", body: JSON.stringify({ expected_version: expectedVersion }) }),

  purgePost: (slug: string, expectedVersion: number): Promise<void> =>
    request(`/api/admin/v1/posts/${encodeURIComponent(slug)}/purge`, { method: "POST", body: JSON.stringify({ expected_version: expectedVersion }) }),

  getPost: (slug: string): Promise<PostDetail> =>
    request<PostDetail>(`/api/admin/v1/posts/${encodeURIComponent(slug)}`),

  createPost: (input: CreatePostInput): Promise<PostDetail> =>
    request<PostDetail>("/api/admin/v1/posts", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  updatePost: (slug: string, input: EditPostInput): Promise<PostDetail> =>
    request<PostDetail>(`/api/admin/v1/posts/${encodeURIComponent(slug)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  publishPost: (slug: string, expectedVersion?: number): Promise<PostDetail> =>
    request<PostDetail>(`/api/admin/v1/posts/${encodeURIComponent(slug)}/publish`, {
      method: "POST",
      body: JSON.stringify({ expected_version: expectedVersion }),
    }),

  unpublishPost: (slug: string, expectedVersion?: number): Promise<PostDetail> =>
    request<PostDetail>(`/api/admin/v1/posts/${encodeURIComponent(slug)}/unpublish`, {
      method: "POST",
      body: JSON.stringify({ expected_version: expectedVersion }),
    }),

  listPages: (): Promise<PageSummary[]> => request<PageSummary[]>("/api/admin/v1/pages"),

  getPage: (slug: string): Promise<PageDetail> =>
    request<PageDetail>(`/api/admin/v1/pages/${encodeURIComponent(slug)}`),

  createPage: (input: CreatePageInput): Promise<PageDetail> =>
    request<PageDetail>("/api/admin/v1/pages", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  updatePage: (slug: string, input: EditPageInput): Promise<PageDetail> =>
    request<PageDetail>(`/api/admin/v1/pages/${encodeURIComponent(slug)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  publishPage: (slug: string, expectedVersion?: number): Promise<PageDetail> =>
    request<PageDetail>(`/api/admin/v1/pages/${encodeURIComponent(slug)}/publish`, {
      method: "POST",
      body: JSON.stringify({ expected_version: expectedVersion }),
    }),

  unpublishPage: (slug: string, expectedVersion?: number): Promise<PageDetail> =>
    request<PageDetail>(`/api/admin/v1/pages/${encodeURIComponent(slug)}/unpublish`, {
      method: "POST",
      body: JSON.stringify({ expected_version: expectedVersion }),
    }),

  /** Page 没有回收站；同时核对 id 与版本，避免 slug 被重新占用后误删新页面。 */
  deletePage: (slug: string, expectedId: string, expectedVersion: number): Promise<void> =>
    request<void>(`/api/admin/v1/pages/${encodeURIComponent(slug)}`, {
      method: "DELETE",
      body: JSON.stringify({ expected_id: expectedId, expected_version: expectedVersion }),
    }),

  /** 账号列表：需 `user.manage` 或 `role.manage`，否则 403 forbidden。 */
  listUsers: (limit?: number, offset?: number): Promise<AdminUser[]> => {
    const query = new URLSearchParams();
    if (limit !== undefined) query.set("limit", String(limit));
    if (offset !== undefined) query.set("offset", String(offset));
    const encoded = query.toString();
    const suffix = encoded.length > 0 ? `?${encoded}` : "";
    return request<AdminUser[]>(`/api/admin/v1/users${suffix}`);
  },

  /**
   * 创建账号（需 `user.manage`）。用户名/邮箱占用是 409，但业务码不同：
   * `username_taken` / `email_taken`，界面据此把错误定位到对应字段。
   */
  createUser: (input: CreateUserInput): Promise<CreatedUser> =>
    request<CreatedUser>("/api/admin/v1/users", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  /** 角色目录：内置 slug 与权限数量；需 `role.manage` 或 `user.manage`。 */
  listRoles: (): Promise<RoleSummary[]> => request<RoleSummary[]>("/api/admin/v1/roles"),

  /**
   * 标签目录：已认证会话即可读（Author 编辑文章要选标签）；
   * 管理（创建/改名/删除）需 `tag.manage`，由后端判定。
   */
  listTags: (): Promise<TagSummary[]> => request<TagSummary[]>("/api/admin/v1/tags"),

  /**
   * 创建标签（需 `tag.manage`）。slug 冲突是 409 `conflict`（创建后 slug 不可改）。
   */
  createTag: (input: CreateTagInput): Promise<TagSummary> =>
    request<TagSummary>("/api/admin/v1/tags", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  /**
   * 改名（需 `tag.manage`；slug 不变，影响全部引用文章）。
   * expected_version 过期是 409 `version_conflict`。
   */
  renameTag: (slug: string, input: RenameTagInput): Promise<TagSummary> =>
    request<TagSummary>(`/api/admin/v1/tags/${encodeURIComponent(slug)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  /**
   * 删除（需 `tag.manage`）。仍被文章引用（含草稿/私密/回收站）时
   * 409 `tag_in_use`——先解除关联再删除；成功返回 204。
   */
  deleteTag: (slug: string, expectedVersion?: number): Promise<void> =>
    request<void>(`/api/admin/v1/tags/${encodeURIComponent(slug)}`, {
      method: "DELETE",
      body: JSON.stringify({ expected_version: expectedVersion }),
    }),

  /**
   * 分配/移除角色（需 `role.manage`；Owner 还需 `ownership.manage`）。
   * 会递增目标用户的 `users.version`，其旧会话立即失效；重复分配是幂等的。
   * 最后一个可登录 Owner 的移除被拒：403 `last_owner`。
   */
  assignRole: (username: string, role: string): Promise<unknown> =>
    request<unknown>(rolePath(username, role), { method: "PUT" }),

  removeRole: (username: string, role: string): Promise<unknown> =>
    request<unknown>(rolePath(username, role), { method: "DELETE" }),

  logout: (): Promise<unknown> => request<unknown>("/auth/logout", { method: "POST" }),
};

/** 角色分配路径：用户名与 slug 都做百分号编码，避免路径段注入。 */
function rolePath(username: string, role: string): string {
  return `/api/admin/v1/users/${encodeURIComponent(username)}/roles/${encodeURIComponent(role)}`;
}

/**
 * 构造登录 URL。`provider` 必须来自 `/auth/providers`（后端不接受缺失 provider）；
 * 无可用提供商时返回 null，由界面提示运维先配置。
 */
export async function loginUrl(next: string): Promise<string | null> {
  try {
    const providers = await api.providers();
    const first = providers[0];
    if (first === undefined) return null;
    return `/auth/login?provider=${encodeURIComponent(first.id)}&next=${encodeURIComponent(next)}`;
  } catch {
    return null;
  }
}

export interface CreateCategoryInput {
  name: string;
  slug: string;
  parent?: string;
  description?: string;
}

export interface UpdateCategoryInput {
  name: string;
  description?: string;
  /** null = 移到根；slug = 移到指定父；缺省保持现状。 */
  parent?: string | null;
  expected_version?: number;
}

/** 分类目录：读取对已登录会话开放；管理需 category.manage。 */
export const categoryApi = {
  list: (): Promise<CategorySummary[]> =>
    request<CategorySummary[]>("/api/admin/v1/categories"),

  /** 创建。slug 冲突是 409 conflict；slug 创建后不可改。 */
  create: (input: CreateCategoryInput): Promise<CategorySummary> =>
    request<CategorySummary>("/api/admin/v1/categories", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  /** 更新（改名/描述/移动父节点）。移动成环是 400 invalid_request。 */
  update: (slug: string, input: UpdateCategoryInput): Promise<CategorySummary> =>
    request<CategorySummary>(`/api/admin/v1/categories/${encodeURIComponent(slug)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  /** 删除：被文章引用或仍有子分类时 409 category_in_use。 */
  remove: (slug: string, expectedVersion?: number): Promise<void> =>
    request<void>(`/api/admin/v1/categories/${encodeURIComponent(slug)}`, {
      method: "DELETE",
      body: JSON.stringify({ expected_version: expectedVersion }),
    }),
};

export interface CreateSeriesInput {
  name: string;
  slug: string;
  description?: string;
}

export interface UpdateSeriesInput {
  name: string;
  description?: string;
  /**
   * 封面的**三态**：缺省 = 不触碰；`null` = 移除封面；id = 设置封面。
   *
   * 系列屏在同一次保存里必须原样带上 `name`（后端要求必填），因此改封面
   * 也就是一次普通的系列更新，只是多带了 `cover_media_id`。
   */
  cover_media_id?: string | null;
  expected_version?: number;
}

export interface SaveSiteSettingsInput {
  title: string;
  description: string;
  /** 站点 logo 媒体 id；PUT 是整组替换，缺省/null = 清除 logo。 */
  logo_media_id?: string | null;
  expected_version?: number;
}

/** 站点设置的 site 分组：读/写都需 `settings.manage`。 */
export const settingsApi = {
  /** 生效值 + 来源 + 版本；未配置时返回环境变量/默认值（version=0）。 */
  get: (): Promise<SiteSettings> => request<SiteSettings>("/api/admin/v1/settings/site"),

  /** 全量替换。expected_version 过期是 409 version_conflict；非法值 400。 */
  save: (input: SaveSiteSettingsInput): Promise<SiteSettings> =>
    request<SiteSettings>("/api/admin/v1/settings/site", {
      method: "PUT",
      body: JSON.stringify(input),
    }),
};

export const themeSettingsApi = {
  get: (): Promise<ThemeSettings> => request<ThemeSettings>("/api/admin/v1/settings/theme"),
  save: (slug: string, expectedVersion: number): Promise<ThemeSettings> =>
    request<ThemeSettings>("/api/admin/v1/settings/theme", {
      method: "PUT",
      body: JSON.stringify({ slug, expected_version: expectedVersion }),
    }),
};

/**
 * 媒体库。
 *
 * 上传是**裸字节体**（不是 multipart）：`?filename=` 只提供展示名，
 * `Content-Type` 用文件自身类型。后端只按文件内容判定格式，因此这里
 * 声明什么类型都不会让非图片通过。
 */
export const mediaApi = {
  list: (page = 1): Promise<MediaPage> =>
    request<MediaPage>(`/api/admin/v1/media?page=${page}`),

  /** 资产详情 + 全部使用位置（删除前提示、删除被拒后定位引用）。 */
  detail: (id: string): Promise<MediaUsageView> =>
    request<MediaUsageView>(`/api/admin/v1/media/${encodeURIComponent(id)}`),

  upload: (file: File): Promise<MediaAsset> =>
    requestBinary<MediaAsset>(
      `/api/admin/v1/media?filename=${encodeURIComponent(file.name)}`,
      {
        method: "POST",
        headers: { "Content-Type": file.type || "application/octet-stream" },
        body: file,
      },
    ),

  /**
   * 删除。仍被内容引用（含草稿/私密/回收站）时 409 `media_in_use`——
   * 先在使用位置里移除引用再删除；成功返回 204。
   */
  remove: (id: string, expectedVersion: number): Promise<void> =>
    request<void>(`/api/admin/v1/media/${encodeURIComponent(id)}`, {
      method: "DELETE",
      body: JSON.stringify({ expected_version: expectedVersion }),
    }),
};

/** 系列目录：读取开放；管理需 series.manage（重排逐篇核验文章授权）。 */
export const seriesApi = {
  list: (): Promise<SeriesSummary[]> =>
    request<SeriesSummary[]>("/api/admin/v1/series"),

  create: (input: CreateSeriesInput): Promise<SeriesSummary> =>
    request<SeriesSummary>("/api/admin/v1/series", {
      method: "POST",
      body: JSON.stringify(input),
    }),

  update: (slug: string, input: UpdateSeriesInput): Promise<SeriesSummary> =>
    request<SeriesSummary>(`/api/admin/v1/series/${encodeURIComponent(slug)}`, {
      method: "PATCH",
      body: JSON.stringify(input),
    }),

  /** 删除：仍被文章引用时 409 series_in_use。 */
  remove: (slug: string, expectedVersion?: number): Promise<void> =>
    request<void>(`/api/admin/v1/series/${encodeURIComponent(slug)}`, {
      method: "DELETE",
      body: JSON.stringify({ expected_version: expectedVersion }),
    }),

  /** 管理目录：系列全部成员（含他人草稿/私密）；需 series.manage。 */
  members: (slug: string): Promise<SeriesMemberRow[]> =>
    request<SeriesMemberRow[]>(
      `/api/admin/v1/series/${encodeURIComponent(slug)}/members`,
    ),

  /** 整体重排（完整排列 + series 版本前提；改他人文章需 any 权限）。 */
  reorder: (
    slug: string,
    orderedPostIds: string[],
    expectedSeriesVersion?: number,
  ): Promise<{ series_version: number; ordered_post_ids: string[] }> =>
    request(`/api/admin/v1/series/${encodeURIComponent(slug)}/reorder`, {
      method: "POST",
      body: JSON.stringify({
        ordered_post_ids: orderedPostIds,
        expected_series_version: expectedSeriesVersion,
      }),
    }),
};
