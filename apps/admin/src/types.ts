/** 与 GET /api/admin/v1/*、GET /auth/providers 的响应契约一致。 */

export type ProviderKind = "oidc" | "github";
export type Visibility = "public" | "private";

/** GET /auth/providers（公开、不含任何配置细节）。 */
export interface ProviderSummary {
  id: string;
  name: string;
  kind: ProviderKind;
}

/** GET /api/admin/v1/me */
export interface Me {
  user_id: string;
  permissions: string[];
  csrf_token: string;
  channel: "session";
}

/** POST /auth/login/password（成功时另下发会话 cookie） */
export interface PasswordLoginResult {
  user_id: string;
  next: string;
}

/** 列表条目：摘要形态，不含正文。tag_ids 在列表中恒为空数组（详情才读取）。 */
export interface PostSummary {
  id: string;
  slug: string;
  title: string;
  status: string;
  visibility: Visibility;
  version: number;
  published_at: string | null;
  updated_at: string;
  author_id: string;
  tag_ids: string[];
}

/** 单篇详情：摘要 + Markdown 源文（编辑器数据源）。 */
export interface PostDetail extends PostSummary {
  excerpt: string | null;
  content: string;
}

/** 页面列表条目：站点级内容，无作者与摘要。 */
export interface PageSummary {
  id: string;
  slug: string;
  title: string;
  status: string;
  visibility: Visibility;
  version: number;
  published_at: string | null;
  updated_at: string;
}

/** 页面详情：摘要 + Markdown 源文（编辑器数据源）。 */
export interface PageDetail extends PageSummary {
  content: string;
}

/** GET /api/admin/v1/users 列表条目（账号管理）。 */
export interface AdminUser {
  id: string;
  username: string;
  email: string | null;
  display_name: string | null;
  deleted: boolean;
  /** 至少一种登录方式（本地密码或外部身份）；Owner 保护看这个谓词。 */
  can_login: boolean;
  /**
   * 全局判定：该账号是最后一个可登录的 Owner，移除其 Owner 角色会被后端拒绝。
   * 由后端按全站计数得出，不受列表分页影响。
   */
  is_last_loginable_owner: boolean;
  password_enabled: boolean;
  external_identities: number;
  roles: string[];
}

/** POST /api/admin/v1/users 成功响应。 */
export interface CreatedUser {
  id: string;
  username: string;
  display_name: string | null;
  created_at: string;
}

/** GET /api/admin/v1/roles 列表条目。 */
export interface RoleSummary {
  slug: string;
  name: string;
  description: string | null;
  /** 内置角色由 seed 保留，普通 API 不可创建/改名/删除。 */
  builtin: boolean;
  permission_count: number;
}

/** GET /api/admin/v1/tags 列表条目（标签目录）。 */
export interface TagSummary {
  id: string;
  slug: string;
  name: string;
  version: number;
  /** 公开文章计数（与公开标签页同口径；草稿/私密/回收站不计入）。 */
  public_post_count: number;
}
