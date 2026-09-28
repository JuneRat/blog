/** Response types inferred from validators checked against generated Rust HTTP DTOs. */
import type { z } from "zod";
import type * as schemas from "./api/schemas";
export type ProviderKind = z.output<typeof schemas.providerKind>;
export type Visibility = z.output<typeof schemas.visibility>;
export type ProviderSummary = z.output<typeof schemas.providerSummary>;
export type Me = z.output<typeof schemas.me>;
export type Profile = z.output<typeof schemas.profile>;
export type PasswordLoginResult = z.output<typeof schemas.passwordLoginResult>;
export type PostSummary = z.output<typeof schemas.postSummary>;
export type PostDetail = z.output<typeof schemas.postDetail>;
export type PageSummary = z.output<typeof schemas.pageSummary>;
export type PageDetail = z.output<typeof schemas.pageDetail>;
export type AdminUser = z.output<typeof schemas.adminUser>;
export type CreatedUser = z.output<typeof schemas.createdUser>;
export type RoleSummary = z.output<typeof schemas.roleSummary>;
export type TagSummary = z.output<typeof schemas.tagSummary>;
export type CategorySummary = z.output<typeof schemas.categorySummary>;
export type SeriesSummary = z.output<typeof schemas.seriesSummary>;
export type SeriesMemberRow = z.output<typeof schemas.seriesMemberRow>;
export type SiteSettingsSource = z.output<typeof schemas.siteSettingsSource>;
export type SiteSettings = z.output<typeof schemas.siteSettings>;
export type ThemeSettings = z.output<typeof schemas.themeSettings>;
export type MediaAsset = z.output<typeof schemas.mediaAsset>;
export type MediaPage = z.output<typeof schemas.mediaPage>;
export type MediaReference = z.output<typeof schemas.mediaReference>;
export type MediaUsageView = z.output<typeof schemas.mediaUsageView>;
export type SeriesPlacement = z.output<typeof schemas.seriesPlacement>;
export type AuditRecord = z.output<typeof schemas.auditRecord>;
export type AuditPage = z.output<typeof schemas.auditPage>;
export type { ContentPage } from "./api/generated";
export type PageTrash = z.output<typeof schemas.pagePage>;
export interface AuditFilter {
  action?: string;
  actor_id?: string;
  without_actor?: boolean;
  target_type?: string;
  target_id?: string;
  from?: string;
  until?: string;
}
export interface ContentListFilter {
  page: number;
  status?: string;
  visibility?: Visibility;
}
