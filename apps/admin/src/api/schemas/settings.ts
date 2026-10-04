import { z } from "zod";
import type * as Wire from "../generated";
import { responseObject as object } from "../contract";
import { string, nullable, count, siteSettingsSource } from "./primitives";

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
  release: string,
  id: string.optional(), config_version: count.optional(), config_schema_version: count.optional(),
});
export const themeSettings = object<Wire.ThemeSettings>()({
  slug: string,
  effective_slug: string,
  fallback_slug: string,
  source: siteSettingsSource,
  version: count,
  available: z.array(themeOption),
});
export const themePackageReport = object<Wire.ThemePackageReport>()({
  slug: string,
  name: string,
  release: string,
  template_count: count,
  asset_count: count,
});
export const previousTheme = object<Wire.PreviousTheme>()({ previous: themePackageReport.nullable() });
export const themePreview = object<Wire.ThemePreview>()({ html: string });
export const retentionSettings = object<Wire.RetentionSettings>()({
  comment_ip_days: count,
  comment_version: count,
  audit_days: count,
  audit_version: count,
});

export const themeConfigValue = z.union([z.boolean(), z.number().int().safe(), z.string(), z.null()]);
export const themeConfigChoice = object<Wire.ThemeConfigChoice>()({ value: string, label: string });
export const themeConfigField = object<Wire.ThemeConfigField>()({
  key: string, type: z.enum(["text", "textarea", "integer", "boolean", "select", "color", "media"]),
  label: string, description: string, group: string, default: themeConfigValue,
  min_length: count.nullable(), max_length: count.nullable(), min: z.number().int().safe().nullable(), max: z.number().int().safe().nullable(),
  options: z.array(themeConfigChoice),
});
export const themeConfigSettings = object<Wire.ThemeConfigSettings>()({
  id: string, slug: string, release: string, fields: z.array(themeConfigField),
  config: z.record(string, themeConfigValue), overrides: z.record(string, themeConfigValue), config_schema_version: count, version: count,
});
