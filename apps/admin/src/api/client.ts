import type { z } from "zod";

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
export function withRequestId(
  message: string,
  requestId: string | null,
): string {
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
    const value = data.error;
    if (typeof value === "string" && value.length > 0) return value;
  }
  return fallback;
}

/** 读取错误响应里的业务码；缺失或非字符串时为 null。 */
function errorCode(data: unknown): string | null {
  if (data !== null && typeof data === "object" && "code" in data) {
    const value = data.code;
    if (typeof value === "string" && value.length > 0) return value;
  }
  return null;
}

/** A successful HTTP status with an incompatible body is a protocol failure, not a retryable write. */
export class ApiProtocolError extends ApiError {
  constructor(status: number, requestId: string | null) {
    super(
      status,
      "服务器响应格式不兼容，请刷新页面核对结果",
      "invalid_response",
      requestId,
    );
    this.name = "ApiProtocolError";
  }
}

async function perform(
  path: string,
  init: RequestInit,
  jsonBody: boolean,
): Promise<Response> {
  const method = (init.method ?? "GET").toUpperCase();
  const headers = new Headers(init.headers);
  if (jsonBody) headers.set("Content-Type", "application/json");
  if (method !== "GET" && method !== "HEAD" && csrfToken !== null)
    headers.set("X-CSRF-Token", csrfToken);
  const response = await fetch(path, {
    ...init,
    headers,
    credentials: "same-origin",
  });
  if (!response.ok) {
    let data: unknown = null;
    try {
      data = await response.json();
    } catch {
      /* HTML and empty errors still preserve status/request ID. */
    }
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
  return response;
}
async function decode<T>(response: Response, schema: z.ZodType<T>): Promise<T> {
  const fail = () =>
    new ApiProtocolError(response.status, response.headers.get("x-request-id"));
  const contentType = response.headers
    .get("content-type")
    ?.split(";", 1)[0]
    .trim()
    .toLowerCase();
  if (contentType !== "application/json") throw fail();
  let data: unknown;
  try {
    data = await response.json();
  } catch {
    throw fail();
  }
  const result = schema.safeParse(data);
  if (!result.success) throw fail();
  return result.data;
}
export async function request<T>(
  schema: z.ZodType<T>,
  path: string,
  init: RequestInit = {},
): Promise<T> {
  return decode(await perform(path, init, init.body !== undefined), schema);
}
export async function requestBinary<T>(
  schema: z.ZodType<T>,
  path: string,
  init: RequestInit,
): Promise<T> {
  return decode(await perform(path, init, false), schema);
}
export async function requestEmpty(
  path: string,
  init: RequestInit = {},
): Promise<void> {
  const response = await perform(path, init, init.body !== undefined);
  if (response.status !== 204 || (await response.text()).length !== 0) {
    throw new ApiProtocolError(
      response.status,
      response.headers.get("x-request-id"),
    );
  }
}
/** Logout follows the server's 303 to the public HTML page; it has no JSON contract. */
export async function requestLogout(): Promise<void> {
  await perform("/auth/logout", { method: "POST" }, false);
}
/** Compile-check request bodies against Rust input DTOs before serializing. */
export function json<T>(value: T): string {
  return JSON.stringify(value);
}
