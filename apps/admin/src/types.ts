/** 与 GET /api/admin/v1/*、GET /auth/providers 的响应契约一致。 */

export type ProviderKind = "oidc" | "github";
export type Visibility = "public" | "private";

/** GET /auth/providers（公开、不含任何配置细节）。 */
export interface ProviderSummary {
  id: string;
  name: string;
  kind: ProviderKind;
}

/** GET /api/admin/v1/me（含本人资料，供头部头像与自助设置使用）。 */
export interface Me {
  user_id: string;
  username: string;
  display_name: string | null;
  avatar_media_id: string | null;
  /** 头像站内地址（`/media/{id}`；null = 无头像）。 */
  avatar_url: string | null;
  permissions: string[];
  csrf_token: string;
  channel: "session";
}

/** PUT /api/admin/v1/me/avatar 的响应（与 `Me` 的资料字段同源）。 */
export interface Profile {
  user_id: string;
  username: string;
  display_name: string | null;
  avatar_media_id: string | null;
  avatar_url: string | null;
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
  category_id: string | null;
  series_id: string | null;
  series_order: number | null;
  /** 封面所引用的媒体资产 id（null = 无封面）；与封面地址同时出现。 */
  cover_media_id: string | null;
  /** 封面站内地址（`/media/{id}`；null = 无封面），由后端随 `cover_media_id` 下发。 */
  cover_url: string | null;
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

/** GET /api/admin/v1/categories 列表条目（分类目录）。 */
export interface CategorySummary {
  id: string;
  slug: string;
  name: string;
  parent_id: string | null;
  description: string | null;
  version: number;
  /** 直接归属的公开文章计数。 */
  pub_post_count: number;
}

/** GET /api/admin/v1/series 列表条目（系列目录）。 */
export interface SeriesSummary {
  id: string;
  slug: string;
  name: string;
  description: string | null;
  version: number;
  /** 成员总数（含草稿/私密——它们保留位置）。 */
  post_count: number;
  pub_post_count: number;
  /** 封面所引用的媒体资产 id（null = 无封面）；与封面地址同时出现。 */
  cover_media_id: string | null;
  /** 封面站内地址（`/media/{id}`；null = 无封面）。 */
  cover_url: string | null;
}

/** 系列成员（重排与目录展示）。 */
export interface SeriesMemberRow {
  id: string;
  slug: string;
  title: string;
  status: string;
  deleted: boolean;
  author_id: string;
  series_order: number | null;
}

/** 当前生效值的来源：database（settings.site 行）或 fallback（环境变量/默认值）。 */
export type SiteSettingsSource = "database" | "fallback";

/** GET/PUT /api/admin/v1/settings/site（读/写都需 settings.manage）。 */
export interface SiteSettings {
  title: string;
  description: string;
  /** 站点 logo 的媒体资产 id（null = 无 logo）。 */
  logo_media_id: string | null;
  /** 站点 logo 站内地址（`/media/{id}`；null = 无 logo）。 */
  logo_url: string | null;
  source: SiteSettingsSource;
  /** site 行版本；未配置为 0（首次保存以此为 expected_version）。 */
  version: number;
}

export interface ThemeSettings {
  slug: string;
  effective_slug: string;
  source: SiteSettingsSource;
  version: number;
  available: { slug: string; name: string }[];
}

/**
 * GET /api/admin/v1/media 列表条目，同时是上传响应。
 *
 * `reference_count` 是全部站内引用数；软删除保留引用，所有图片链接独立公开。
 */
export interface MediaAsset {
  id: string;
  original_name: string;
  mime: string;
  byte_size: number;
  width: number;
  height: number;
  deleted_at: string | null;
  version: number;
  created_at: string;
  owner_id: string | null;
  owner_display: string;
  /** 站内地址：正文插入与「复制地址」共用。 */
  url: string;
  reference_count: number;
}

/** GET /api/admin/v1/media?page=N */
export interface MediaPage {
  items: MediaAsset[];
  total: number;
  page: number;
  per_page: number;
}

/** 一处使用位置（已按内容阅读权限过滤）。 */
export interface MediaReference {
  /** 引用来源：文章/页面正文、系列封面、用户头像或站点 logo。 */
  kind: "post" | "page" | "series" | "user" | "site";
  content_id: string;
  slug: string;
  title: string;
  status: string;
  visibility: Visibility;
  /** 来源内容是否在回收站。 */
  deleted: boolean;
  /** 来源内容是否公开可读；与图片链接公开性无关。 */
  public: boolean;
}

/** GET /api/admin/v1/media/{id}：资产详情 + 调用者有权查看的使用位置。 */
export interface MediaUsageView {
  media: MediaAsset;
  /** 按调用者内容权限过滤后的使用位置（Post own/any、Page 站点级）。 */
  references: MediaReference[];
  /**
   * 存在但调用者无权查看的引用数。
   *
   * 引用计数是全局的（决定能否删除），展示必须过滤——差额如实返回，
   * 否则界面会显示「被 3 处引用」却只列出 1 处。
   */
  hidden_references: number;
}
