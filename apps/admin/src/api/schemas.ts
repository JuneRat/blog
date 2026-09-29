import { z } from "zod";
import type * as Wire from "./generated";

import { responseObject as object } from "./contract";

const string = z.string();
const nullable = string.nullable();
const integer = z.number().int().safe();
const count = integer.nonnegative();
export const visibility = z.enum(["public", "private"]);
export const providerKind = z.enum(["oidc", "github"]);
export const siteSettingsSource = z.enum(["database", "fallback"]);
const accountStatus = z.enum(["active", "disabled"]);
export const providerSummary = object<Wire.ProviderSummary>()({
  id: string,
  name: string,
  kind: providerKind,
});
export const profile = object<Wire.Profile>()({
  user_id: string,
  username: string,
  display_name: nullable,
  bio: nullable,
  version: count,
  avatar_media_id: nullable,
  avatar_url: nullable,
});
export const me = object<Wire.Me>()({
  ...profile.shape,
  time_zone: string,
  permissions: z.array(string),
  csrf_token: string,
  channel: z.literal("session"),
});
export const passwordLoginResult = object<Wire.PasswordLoginResult>()({
  user_id: string,
  next: string,
});
export const passwordChangeResult = object<Wire.PasswordChangeResult>()({
  user_id: string,
  csrf_token: string,
});
export const pageSummary = object<Wire.PageSummary>()({
  id: string,
  slug: string,
  title: string,
  status: string,
  visibility,
  version: count,
  published_at: nullable,
  updated_at: string,
});
export const pageDetail = object<Wire.PageDetail>()({
  ...pageSummary.shape,
  content: string,
});
export const postSummary = object<Wire.PostSummary>()({
  author_username: string,
  ...pageSummary.shape,
  author_id: string,
});
export const seriesPlacement = object<Wire.SeriesPlacement>()({
  series_id: string,
  position: count.max(2147483647),
});
export const postDetail = object<Wire.PostDetail>()({
  ...pageSummary.shape,
  author_id: string,
  tag_ids: z.array(string),
  category_id: nullable,
  series: z.array(seriesPlacement),
  cover_media_id: nullable,
  cover_url: nullable,
  excerpt: nullable,
  content: string,
});
export const postPage = object<Wire.ContentPage<Wire.PostSummary>>()({
  items: z.array(postSummary),
  total: count,
  page: count,
  per_page: count,
});
export const pagePage = object<Wire.ContentPage<Wire.PageSummary>>()({
  items: z.array(pageSummary),
  total: count,
  page: count,
  per_page: count,
});
export const adminUser = object<Wire.AdminUser>()({
  id: string,
  username: string,
  email: nullable,
  display_name: nullable,
  status: accountStatus,
  version: count,
  deleted: z.boolean(),
  can_login: z.boolean(),
  is_last_loginable_admin: z.boolean(),
  password_enabled: z.boolean(),
  external_identities: count,
  roles: z.array(string),
});
export const createdUser = object<Wire.CreatedUser>()({
  id: string,
  username: string,
  display_name: nullable,
  created_at: string,
});
export const userStatusResult = object<Wire.UserStatusResult>()({
  id: string,
  status: accountStatus,
  version: count,
});
export const roleSummary = object<Wire.RoleSummary>()({
  slug: string,
  name: string,
  description: nullable,
  builtin: z.boolean(),
  permission_count: count,
});
export const tagSummary = object<Wire.TagSummary>()({
  id: string,
  slug: string,
  name: string,
  version: count,
  public_post_count: count,
});
export const categorySummary = object<Wire.CategorySummary>()({
  id: string,
  slug: string,
  name: string,
  parent_id: nullable,
  description: nullable,
  version: count,
  pub_post_count: count,
});
export const seriesSummary = object<Wire.SeriesSummary>()({
  id: string,
  slug: string,
  name: string,
  description: nullable,
  version: count,
  post_count: count,
  pub_post_count: count,
  cover_media_id: nullable,
  cover_url: nullable,
});
export const seriesMemberRow = object<Wire.SeriesMemberRow>()({
  id: string,
  slug: string,
  title: string,
  status: string,
  deleted: z.boolean(),
  visibility,
  author_id: string,
  position: integer,
});
export const reorderSeriesResult = object<Wire.ReorderSeriesResult>()({
  series_version: count,
  ordered_post_ids: z.array(string),
});
export const navigationItem = object<Wire.NavigationItem>()({
  label: string, page_slug: string, placement: z.enum(["header", "footer"]),
});
export const siteSettings = object<Wire.SiteSettings>()({
  home_page_size: count,
  navigation: z.array(navigationItem),
  time_zone: string,
  time_zones: z.array(string),
  title: string,
  description: string,
  logo_media_id: nullable,
  logo_url: nullable,
  source: siteSettingsSource,
  version: count,
});
export const themeOption = object<Wire.ThemeOption>()({
  slug: string,
  name: string,
});
export const themeSettings = object<Wire.ThemeSettings>()({
  slug: string,
  effective_slug: string,
  source: siteSettingsSource,
  version: count,
  available: z.array(themeOption),
});
export const retentionSettings = object<Wire.RetentionSettings>()({
  comment_ip_days: count,
  comment_version: count,
  audit_days: count,
  audit_version: count,
});
export const mediaAsset = object<Wire.MediaAsset>()({
  id: string,
  original_name: string,
  mime: string,
  byte_size: count,
  width: count,
  height: count,
  deleted_at: nullable,
  version: count,
  created_at: string,
  owner_id: nullable,
  owner_display: string,
  url: string,
  reference_count: count,
});
export const mediaPage = object<Wire.MediaPage>()({
  items: z.array(mediaAsset),
  total: count,
  page: count,
  per_page: count,
});
export const mediaReference = object<Wire.MediaReference>()({
  kind: z.enum(["post", "page", "series", "user", "site"]),
  content_id: string,
  slug: string,
  title: string,
  status: string,
  visibility,
  deleted: z.boolean(),
  public: z.boolean(),
});
export const mediaUsageView = object<Wire.MediaUsageView>()({
  media: mediaAsset,
  references: z.array(mediaReference),
  hidden_references: count,
});
export const auditField = object<Wire.AuditField>()({
  key: string,
  value: string,
});
export const auditRecord = object<Wire.AuditRecord>()({
  id: string,
  created_at: string,
  actor_id: nullable,
  actor_display: nullable,
  ip_address: nullable,
  action: string,
  target_type: string,
  target_id: string,
  summary: z.array(auditField),
});
export const auditPage = object<Wire.AuditPage>()({
  items: z.array(auditRecord),
  next_cursor: nullable,
});
export const commentItem = object<Wire.CommentItem>()({
  moderation_reason: nullable,
  id: string,
  post_id: string,
  post_slug: string,
  post_title: string,
  parent_id: nullable,
  root_id: nullable,
  parent_nickname: nullable,
  author_email: nullable,
  ip_address: nullable,
  content_html: string,
  nickname: string,
  body: string,
  is_author: z.boolean(),
  status: string,
  version: count,
  created_at: string,
});
export const commentPage = object<Wire.CommentPage>()({
  items: z.array(commentItem),
  total: count,
  enabled: z.boolean(),
});
export const commentPolicy = object<Wire.CommentPolicy>()({
  moderation: z.enum(['all', 'guests', 'first_comment', 'none']).nullish(),
  enabled: z.boolean(),
  version: count,
});
export const previewResult = object<Wire.PreviewResult>()({
  content_html: string,
});
export const messageResult = object<Wire.MessageResult>()({ message: string });
export const commentSubmissionResult = object<Wire.CommentSubmissionResult>()({
  message: string,
  status: z.enum(['pending', 'approved']),
});
export const array = z.array;

export const registrationStatus = object<Wire.RegistrationStatus>()({ enabled: z.boolean() });
export const accessSettings = object<Wire.AccessSettings>()({
  registration_enabled: z.boolean(), guest_comments_enabled: z.boolean(), version: count,
});
