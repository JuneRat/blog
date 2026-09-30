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
