import { z } from "zod";
import type * as Wire from "./generated";
import * as s from "./schemas/identity";
import { json, request, requestEmpty, requestLogout } from "./client";
import type {
  AdminUser,
  CreatedUser,
  Me,
  PasswordLoginResult,
  Profile,
  ProviderSummary,
  RoleSummary,
  UserPage,
} from "../types";
import type {
  PasswordLoginInput,
  CreateUserInput,
  UpdateProfileInput,
  ChangePasswordInput,
} from "./generated";

export const identityApi = {
  registrationStatus: () => request(s.registrationStatus, "/auth/register"),
  register: (input: Wire.RegistrationInput) => request(s.messageResult, "/auth/register", {method: "POST", body: json<Wire.RegistrationInput>(input)}),
  accessSettings: () => request(s.accessSettings, "/api/admin/v1/access-settings"),
  saveAccessSettings: (input: Wire.AccessSettings) => request(s.accessSettings, "/api/admin/v1/access-settings", {method: "PUT", body: json<Wire.AccessSettings>(input)}),
  me: (): Promise<Me> => request(s.me, "/api/admin/v1/me"),

  setOwnAvatar: (avatarMediaId: string | null, expectedVersion: number): Promise<Profile> =>
    request(s.profile, "/api/admin/v1/me/avatar", {
      method: "PUT",
      body: json<Wire.SetAvatarInput>({ avatar_media_id: avatarMediaId, expected_version: expectedVersion }),
    }),

  updateOwnProfile: (input: UpdateProfileInput): Promise<Profile> =>
    request(s.profile, "/api/admin/v1/me/profile", {
      method: "PUT",
      body: json<UpdateProfileInput>(input),
    }),

  changeOwnPassword: (
    input: ChangePasswordInput,
  ): Promise<{ user_id: string; csrf_token: string }> =>
    request(s.passwordChangeResult, "/api/admin/v1/me/password", {
      method: "POST",
      body: json<ChangePasswordInput>(input),
    }),

  providers: (): Promise<ProviderSummary[]> =>
    request(z.array(s.providerSummary), "/auth/providers"),

  loginWithPassword: (
    input: PasswordLoginInput,
  ): Promise<PasswordLoginResult> =>
    request(s.passwordLoginResult, "/auth/login/password", {
      method: "POST",
      body: json<PasswordLoginInput>(input),
    }),

  listUsers: (page = 1, perPage = 50): Promise<UserPage> => {
    const query = new URLSearchParams();
    query.set("page", String(page));
    query.set("per_page", String(perPage));
    return request(s.userPage, `/api/admin/v1/users?${query}`);
  },

  createUser: (input: CreateUserInput): Promise<CreatedUser> =>
    request(s.createdUser, "/api/admin/v1/users", {
      method: "POST",
      body: json<CreateUserInput>(input),
    }),

  changeUserStatus: (
    id: string,
    status: AdminUser["status"],
    expectedVersion: number,
  ): Promise<{ id: string; status: AdminUser["status"]; version: number }> =>
    request(
      s.userStatusResult,
      `/api/admin/v1/users/${encodeURIComponent(id)}/status`,
      {
        method: "PUT",
        body: json<Wire.ChangeStatusInput>({
          status,
          expected_version: expectedVersion,
        }),
      },
    ),

  listRoles: (): Promise<RoleSummary[]> =>
    request(z.array(s.roleSummary), "/api/admin/v1/roles"),

  assignRole: (username: string, role: string): Promise<unknown> =>
    requestEmpty(rolePath(username, role), { method: "PUT" }),

  removeRole: (username: string, role: string): Promise<unknown> =>
    requestEmpty(rolePath(username, role), { method: "DELETE" }),

  logout: (): Promise<unknown> => requestLogout(),
};

function rolePath(username: string, role: string): string {
  return `/api/admin/v1/users/${encodeURIComponent(username)}/roles/${encodeURIComponent(role)}`;
}

/**
 * 构造登录 URL。`provider` 必须来自 `/auth/providers`（后端不接受缺失 provider）；
 * 无可用提供商时返回 null，由界面提示运维先配置。
 */
export async function loginUrl(next: string): Promise<string | null> {
  try {
    const providers = await identityApi.providers();
    const first = providers[0];
    if (first === undefined) return null;
    return `/auth/login?provider=${encodeURIComponent(first.id)}&next=${encodeURIComponent(next)}`;
  } catch {
    return null;
  }
}
