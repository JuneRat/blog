import type * as Wire from "./generated";
import * as s from "./schemas";
import { json, request } from "./client";
import type { SiteSettings, ThemeSettings } from "../types";
import type { SaveSiteSettingsInput, RetentionSettings } from "./generated";

export const settingsApi = {
  /** 生效值 + 来源 + 版本；未配置时返回内置默认值（version=0）。 */
  get: (): Promise<SiteSettings> =>
    request(s.siteSettings, "/api/admin/v1/settings/site"),

  /** 全量替换。expected_version 过期是 409 version_conflict；非法值 400。 */
  save: (input: SaveSiteSettingsInput): Promise<SiteSettings> =>
    request(s.siteSettings, "/api/admin/v1/settings/site", {
      method: "PUT",
      body: JSON.stringify(input),
    }),
};

export const themeSettingsApi = {
  get: (): Promise<ThemeSettings> =>
    request(s.themeSettings, "/api/admin/v1/settings/theme"),
  save: (slug: string, expectedVersion: number): Promise<ThemeSettings> =>
    request(s.themeSettings, "/api/admin/v1/settings/theme", {
      method: "PUT",
      body: json<Wire.SaveThemeSettingsInput>({
        slug,
        expected_version: expectedVersion,
      }),
    }),
};

export const retentionApi = {
  get: (): Promise<RetentionSettings> =>
    request(s.retentionSettings, "/api/admin/v1/settings/retention"),
  save: (input: RetentionSettings): Promise<RetentionSettings> =>
    request(s.retentionSettings, "/api/admin/v1/settings/retention", {
      method: "PUT",
      body: JSON.stringify(input),
    }),
};
