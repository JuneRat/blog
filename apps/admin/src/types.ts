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

/** 列表条目：摘要形态，不含正文。 */
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
}

/** 单篇详情：摘要 + Markdown 源文（编辑器数据源）。 */
export interface PostDetail extends PostSummary {
  excerpt: string | null;
  content: string;
}
