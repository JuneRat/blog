import { StrictMode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { App } from "./App";
import { AuthProvider } from "./auth";
import { setCsrfToken, setUnauthorizedHandler } from "./api/client";
import type { CommentItem } from "./api/generated";
import type { Me } from "./types";
import { paths } from "./router";
import { jsonResponse } from "../tests/httpFixtures";

afterEach(() => {
  cleanup();
  setCsrfToken(null);
  setUnauthorizedHandler(null);
  vi.unstubAllGlobals();
});

it("discards private cached comments when an expired session logs in as another account", async () => {
  let account = "account-a";
  let expired = false;
  let finishComments!: (response: Response) => void;
  const nextComments = new Promise<Response>(resolve => { finishComments = resolve; });
  const item: CommentItem = {
    id: "comment-a", post_id: "post-a", post_slug: "post-a", post_title: "A 的文章",
    parent_id: null, root_id: null, parent_nickname: null, nickname: "A 的访客",
    author_email: "private-a@example.com", ip_address: "198.51.100.2",
    content_html: "<p>A 的待审内容</p>", body: "A 的待审内容", status: "pending",
    moderation_reason: "first_comment", version: 1, is_author: false,
    created_at: "2026-09-29T00:00:00Z",
  };
  vi.stubGlobal("fetch", vi.fn(async (path: string) => {
    if (path === "/auth/providers") return jsonResponse([]);
    if (path === "/auth/register") return jsonResponse({ enabled: false });
    if (path === "/api/admin/v1/me") {
      const me: Me = {
        user_id: account, username: account, display_name: null, bio: null,
        avatar_media_id: null, avatar_url: null, version: 1, time_zone: "UTC",
        permissions: ["post.update"], csrf_token: `csrf-${account}`, channel: "session",
      };
      return jsonResponse(me);
    }
    if (path === "/auth/login/password") {
      account = "account-b";
      return jsonResponse({ user_id: account, next: "/admin/" });
    }
    if (path.startsWith("/api/admin/v1/comments?")) {
      if (account === "account-b") return (await nextComments).clone();
      return expired
        ? jsonResponse({ code: "unauthenticated", error: "会话过期" }, 401)
        : jsonResponse({ items: [item], total: 1, page: 1, per_page: 20, enabled: true });
    }
    throw new Error(`Unexpected request: ${path}`);
  }));
  window.history.replaceState(null, "", paths.comments);
  render(<StrictMode><AuthProvider><App /></AuthProvider></StrictMode>);
  await screen.findByText(/private-a@example.com/);
  expired = true;
  fireEvent.click(screen.getByRole("button", { name: "刷新" }));
  await screen.findByLabelText("用户名或邮箱");
  fireEvent.change(screen.getByLabelText("用户名或邮箱"), { target: { value: "account-b" } });
  fireEvent.change(screen.getByLabelText("密码"), { target: { value: "harbor-lantern-2026" } });
  fireEvent.click(screen.getByRole("button", { name: "登录" }));
  await screen.findByText("正在加载评论…");
  expect(screen.queryByText(/private-a@example.com/)).toBeNull();
  await act(async () => finishComments(jsonResponse({ code: "forbidden", error: "无权访问" }, 403)));
  await screen.findByText(/无权/);
  expect(screen.queryByText("A 的待审内容")).toBeNull();
});
