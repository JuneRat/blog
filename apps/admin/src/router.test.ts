import { describe, expect, it } from "vitest";
import { parseRoute } from "./router";

describe("parseRoute", () => {
  it("解析文章与页面路由，且编辑地址带 /edit 后缀", () => {
    expect(parseRoute("/admin/")).toEqual({ name: "list" });
    expect(parseRoute("/admin/posts/new")).toEqual({ name: "postNew" });
    expect(parseRoute("/admin/posts/hello/edit")).toEqual({
      name: "postEdit",
      id: "hello",
    });
    // 显式编辑路径与新建路径保持独立。
    expect(parseRoute("/admin/posts/new/edit")).toEqual({
      name: "postEdit",
      id: "new",
    });

    expect(parseRoute("/admin/pages")).toEqual({ name: "pageList" });
    expect(parseRoute("/admin/pages/new")).toEqual({ name: "pageNew" });
    expect(parseRoute("/admin/pages/about/edit")).toEqual({
      name: "pageEdit",
      id: "about",
    });
    expect(parseRoute("/admin/pages/new/edit")).toEqual({
      name: "pageEdit",
      id: "new",
    });

    expect(parseRoute("/admin/tags")).toEqual({ name: "tagList" });
    expect(parseRoute("/admin/users")).toEqual({ name: "userList" });
    expect(parseRoute("/admin/roles")).toEqual({ name: "roleList" });
    expect(parseRoute("/admin/settings")).toEqual({ name: "settings" });
    expect(parseRoute("/admin/audit-logs")).toEqual({ name: "auditLogs" });
  });

  it.each(["posts", "pages"])("%s 编辑地址缺少 /edit 后缀时进入 invalid", (section) => {
    const id = "0195c98a-6430-7000-8000-000000000001";
    expect(parseRoute(`/admin/${section}/${id}`)).toEqual({ name: "invalid" });
  });

  it("多余路径段与畸形编码进入 invalid，不静默进入别的页面", () => {
    expect(parseRoute("/admin/posts/new/x")).toEqual({ name: "invalid" });
    expect(parseRoute("/admin/pages/about/edit/x")).toEqual({ name: "invalid" });
    expect(parseRoute("/admin/pages/%/edit")).toEqual({ name: "invalid" });
    expect(parseRoute("/admin/unknown")).toEqual({ name: "invalid" });
    // 标签/用户/角色/设置是固定单段：多余段不静默忽略。
    expect(parseRoute("/admin/tags/rust")).toEqual({ name: "invalid" });
    expect(parseRoute("/admin/users/author")).toEqual({ name: "invalid" });
    expect(parseRoute("/admin/roles/owner")).toEqual({ name: "invalid" });
    expect(parseRoute("/admin/settings/site")).toEqual({ name: "invalid" });
    expect(parseRoute("/admin/audit-logs/edit")).toEqual({ name: "invalid" });
  });
});
