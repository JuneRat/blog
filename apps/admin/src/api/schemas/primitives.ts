import { z } from "zod";

export const string = z.string();
export const nullable = string.nullable();
export const integer = z.number().int().safe();
export const count = integer.nonnegative();
export const visibility = z.enum(["public", "private"]);
export const providerKind = z.enum(["oidc", "github"]);
export const siteSettingsSource = z.enum(["database", "fallback"]);
export const accountStatus = z.enum(["active", "disabled"]);
export const array = z.array;
