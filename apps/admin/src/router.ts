import { useSyncExternalStore } from "react";
import { NavigationHistory } from "./navigationHistory";
import type { HistoryBlocker } from "./navigationHistory";

/**
 * 后台内部路由：
 * - 文章：`/admin/`（列表）、`/admin/posts/new`、`/admin/posts/{id}/edit`
 * - 页面：`/admin/pages`（列表）、`/admin/pages/new`、`/admin/pages/{id}/edit`
 * - 标签：`/admin/tags`
 * - 媒体库：`/admin/media`
 * - 用户与角色：`/admin/users`、`/admin/roles`
 * - 站点设置：`/admin/settings`
 * - 主题与插件管理：`/admin/themes`、`/admin/plugins`
 *
 * 管理地址使用稳定内容 ID；slug 只用于公开地址，改名不会改变编辑页身份。
 */
export type Route =
  | { name: "list" }
  | { name: "comments" }
  | { name: "postTrash" }
  | { name: "postNew" }
  | { name: "postEdit"; id: string }
  | { name: "pageList" }
  | { name: "pageNew" }
  | { name: "pageEdit"; id: string }
  | { name: "tagList" }
  | { name: "mediaLibrary" }
  | { name: "categoryList" }
  | { name: "pageTrash" }
  | { name: "seriesList" }
  | { name: "userList" }
  | { name: "profile" }
  | { name: "roleList" }
  | { name: "settings" }
  | { name: "themes" }
  | { name: "plugins" }
  | { name: "tasks" }
  | { name: "auditLogs" }
  /** 畸形或多余的路径段：显示提示而不是白屏/静默进入别的页面。 */
  | { name: "invalid" };

const BASE = "/admin";

/**
 * 解码单个路径片段。畸形的百分号编码（例如 `/admin/posts/%`）会抛 `URIError`；
 * 这里返回 null 交由路由判定，避免在 render 期间抛出导致整页白屏。
 */
function decodeSegment(segment: string): string | null {
  try {
    return decodeURIComponent(segment);
  } catch {
    return null;
  }
}

/** 编辑地址只接受 `{section}/{id}/edit`。 */
function editId(segments: string[]): string | null {
  if (segments.length === 3 && segments[2] === "edit") {
    const id = decodeSegment(segments[1]);
    return id !== null && id.length > 0 ? id : null;
  }
  return null;
}

/**
 * 解析后台内部路由。只接受固定形状，**多余路径段不再被静默忽略**；
 * 其余（含畸形编码）→ invalid。
 */
export function parseRoute(pathname: string): Route {
  const underBase = pathname === BASE || pathname.startsWith(`${BASE}/`);
  const rest = underBase ? pathname.slice(BASE.length) : pathname;
  const segments = rest.split("/").filter((segment) => segment.length > 0);

  if (segments[0] === "comments") return segments.length === 1 ? { name: "comments" } : { name: "invalid" };
  if (segments.length === 0) return { name: "list" };
  if (segments[0] === "page-trash") return segments.length === 1 ? { name: "pageTrash" } : { name: "invalid" };
  if (segments[0] === "trash") return segments.length === 1 ? { name: "postTrash" } : { name: "invalid" };

  // 标签/用户/角色是固定单段路由：多余路径段不静默忽略。
  if (segments[0] === "tags") {
    return segments.length === 1 ? { name: "tagList" } : { name: "invalid" };
  }
  if (segments[0] === "media") {
    return segments.length === 1 ? { name: "mediaLibrary" } : { name: "invalid" };
  }
  if (segments[0] === "categories") {
    return segments.length === 1 ? { name: "categoryList" } : { name: "invalid" };
  }
  if (segments[0] === "series") {
    return segments.length === 1 ? { name: "seriesList" } : { name: "invalid" };
  }
  if (segments[0] === "users") {
    return segments.length === 1 ? { name: "userList" } : { name: "invalid" };
  }
  if (segments[0] === "profile") {
    return segments.length === 1 ? { name: "profile" } : { name: "invalid" };
  }
  if (segments[0] === "roles") {
    return segments.length === 1 ? { name: "roleList" } : { name: "invalid" };
  }
  if (segments[0] === "settings") {
    return segments.length === 1 ? { name: "settings" } : { name: "invalid" };
  }
  if (segments[0] === "themes") {
    return segments.length === 1 ? { name: "themes" } : { name: "invalid" };
  }
  if (segments[0] === "plugins") {
    return segments.length === 1 ? { name: "plugins" } : { name: "invalid" };
  }
  if (segments[0] === "tasks") {
    return segments.length === 1 ? { name: "tasks" } : { name: "invalid" };
  }
  if (segments[0] === "audit-logs") {
    return segments.length === 1 ? { name: "auditLogs" } : { name: "invalid" };
  }

  if (segments[0] === "pages") {
    if (segments.length === 1) return { name: "pageList" };
    if (segments.length === 2 && segments[1] === "new") return { name: "pageNew" };
    const id = editId(segments);
    return id === null ? { name: "invalid" } : { name: "pageEdit", id };
  }

  if (segments[0] !== "posts") return { name: "invalid" };
  if (segments.length === 2 && segments[1] === "new") return { name: "postNew" };
  const id = editId(segments);
  return id === null ? { name: "invalid" } : { name: "postEdit", id };
}

export const paths = {
  list: `${BASE}/`,
  comments: `${BASE}/comments`,
  newPost: `${BASE}/posts/new`,
  postTrash: `${BASE}/trash`,
  pageTrash: `${BASE}/page-trash`,
  editPost: (id: string): string => `${BASE}/posts/${encodeURIComponent(id)}/edit`,
  pages: `${BASE}/pages`,
  newPage: `${BASE}/pages/new`,
  editPage: (id: string): string => `${BASE}/pages/${encodeURIComponent(id)}/edit`,
  tags: `${BASE}/tags`,
  media: `${BASE}/media`,
  categories: `${BASE}/categories`,
  series: `${BASE}/series`,
  users: `${BASE}/users`,
  profile: `${BASE}/profile`,
  roles: `${BASE}/roles`,
  settings: `${BASE}/settings`,
  themes: `${BASE}/themes`,
  plugins: `${BASE}/plugins`,
  tasks: `${BASE}/tasks`,
  auditLogs: `${BASE}/audit-logs`,
};

/**
 * 使用 History API 的最小路由；服务端对 /admin/* 深链回退到 index.html。
 * `replace` 用于删除后等无需保留旧历史项的跳转。
 */
let history: NavigationHistory | null = null;
let consumers = 0;

function getHistory(): NavigationHistory {
  history ??= new NavigationHistory(window);
  return history;
}

function release(unsubscribe: () => void): () => void {
  consumers += 1;
  return () => {
    unsubscribe();
    consumers -= 1;
    if (consumers === 0) {
      history?.dispose();
      history = null;
    }
  };
}

function subscribe(listener: () => void): () => void {
  return release(getHistory().subscribe(listener));
}

export function blockHistoryNavigation(blocker: HistoryBlocker): () => void {
  return release(getHistory().block(blocker));
}

export function navigate(to: string, options: { replace?: boolean } = {}): void {
  getHistory().navigate(to, options.replace);
}

const getPathname = (): string => getHistory().getPathname();

export function useRoute(): Route {
  return parseRoute(useSyncExternalStore(subscribe, getPathname));
}

const getURL = (): string => getHistory().getURL();
export function useLocation(): string {
  return useSyncExternalStore(subscribe, getURL);
}
