import { useEffect, useState } from "react";

/**
 * 后台内部路由：
 * - 文章：`/admin/`（列表）、`/admin/posts/new`、`/admin/posts/{slug}/edit`
 * - 页面：`/admin/pages`（列表）、`/admin/pages/new`、`/admin/pages/{slug}/edit`
 * - 标签：`/admin/tags`
 * - 用户与角色：`/admin/users`、`/admin/roles`
 *
 * 编辑页带 `/edit` 后缀，使 slug 为 `new` 的内容（`/admin/posts/new/edit`）
 * 不再与新建页（`/admin/posts/new`）相撞——slug 校验对 `new` 是合法的，
 * 冲突必须在路由层解决，而不是靠前端补一段保留字校验。
 */
export type Route =
  | { name: "list" }
  | { name: "postNew" }
  | { name: "postEdit"; slug: string }
  | { name: "pageList" }
  | { name: "pageNew" }
  | { name: "pageEdit"; slug: string }
  | { name: "tagList" }
  | { name: "categoryList" }
  | { name: "userList" }
  | { name: "roleList" }
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

/** `{section}/{slug}` 与 `{section}/{slug}/edit` 两种可接受形状。 */
function editSlug(segments: string[]): string | null {
  if (segments.length === 2 || (segments.length === 3 && segments[2] === "edit")) {
    const slug = decodeSegment(segments[1]);
    return slug !== null && slug.length > 0 ? slug : null;
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

  if (segments.length === 0) return { name: "list" };

  // 标签/用户/角色是固定单段路由：多余路径段不静默忽略。
  if (segments[0] === "tags") {
    return segments.length === 1 ? { name: "tagList" } : { name: "invalid" };
  }
  if (segments[0] === "categories") {
    return segments.length === 1 ? { name: "categoryList" } : { name: "invalid" };
  }
  if (segments[0] === "users") {
    return segments.length === 1 ? { name: "userList" } : { name: "invalid" };
  }
  if (segments[0] === "roles") {
    return segments.length === 1 ? { name: "roleList" } : { name: "invalid" };
  }

  if (segments[0] === "pages") {
    if (segments.length === 1) return { name: "pageList" };
    if (segments.length === 2 && segments[1] === "new") return { name: "pageNew" };
    const slug = editSlug(segments);
    return slug === null ? { name: "invalid" } : { name: "pageEdit", slug };
  }

  if (segments[0] !== "posts") return { name: "invalid" };
  if (segments.length === 2 && segments[1] === "new") return { name: "postNew" };
  const slug = editSlug(segments);
  return slug === null ? { name: "invalid" } : { name: "postEdit", slug };
}

export const paths = {
  list: `${BASE}/`,
  newPost: `${BASE}/posts/new`,
  editPost: (slug: string): string => `${BASE}/posts/${encodeURIComponent(slug)}/edit`,
  pages: `${BASE}/pages`,
  newPage: `${BASE}/pages/new`,
  editPage: (slug: string): string => `${BASE}/pages/${encodeURIComponent(slug)}/edit`,
  tags: `${BASE}/tags`,
  categories: `${BASE}/categories`,
  users: `${BASE}/users`,
  roles: `${BASE}/roles`,
};

/**
 * 使用 History API 的最小路由；服务端对 /admin/* 深链回退到 index.html。
 * `replace` 用于 slug 改名：不应在历史里留下一个已不存在的旧地址。
 */
export function navigate(to: string, options: { replace?: boolean } = {}): void {
  if (window.location.pathname === to) return;
  if (options.replace === true) {
    window.history.replaceState(null, "", to);
  } else {
    window.history.pushState(null, "", to);
  }
  window.dispatchEvent(new PopStateEvent("popstate"));
}

export function useRoute(): Route {
  const [route, setRoute] = useState<Route>(() => parseRoute(window.location.pathname));
  useEffect(() => {
    const update = (): void => setRoute(parseRoute(window.location.pathname));
    window.addEventListener("popstate", update);
    return () => window.removeEventListener("popstate", update);
  }, []);
  return route;
}
