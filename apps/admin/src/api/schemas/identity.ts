import { z } from "zod";
import type * as Wire from "../generated";
import { responseObject as object } from "../contract";
import { string, nullable, count, providerKind, accountStatus } from "./primitives";

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
export const userPage = object<Wire.UserPage>()({
  items: z.array(adminUser),
  total: count,
  page: count,
  per_page: count,
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
export const messageResult = object<Wire.MessageResult>()({ message: string });
export const registrationStatus = object<Wire.RegistrationStatus>()({ enabled: z.boolean() });
export const accessSettings = object<Wire.AccessSettings>()({
  registration_enabled: z.boolean(), guest_comments_enabled: z.boolean(), version: count,
});
