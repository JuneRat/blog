import type {
  Me,
  PageDetail,
  PageSummary,
  PasswordLoginResult,
  PostDetail,
  PostSummary,
  ProviderSummary,
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

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const method = (init.method ?? "GET").toUpperCase();
  const headers = new Headers(init.headers);
  if (init.body !== undefined) headers.set("Content-Type", "application/json");
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

export interface CreatePostInput {
  slug?: string;
  title: string;
  excerpt?: string;
  content: string;
  visibility: Visibility;
}

export interface EditPostInput {
  new_slug?: string;
  title?: string;
  excerpt?: string;
  content?: string;
  visibility?: Visibility;
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

export const api = {
  me: (): Promise<Me> => request<Me>("/api/admin/v1/me"),

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

  logout: (): Promise<unknown> => request<unknown>("/auth/logout", { method: "POST" }),
};

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
