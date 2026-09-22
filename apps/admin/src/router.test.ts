import { describe, expect, it } from "vitest";
import { parseRoute } from "./router";

describe("parseRoute", () => {
  it("解析文章与页面路由，且编辑地址带 /edit 后缀", () => {
    expect(parseRoute("/admin/")).toEqual({ name: "list" });
    expect(parseRoute("/admin/posts/new")).toEqual({ name: "postNew" });
    expect(parseRoute("/admin/posts/hello/edit")).toEqual({
      name: "postEdit",
      slug: "hello",
    });
    // slug 恰好是 new 的内容不会与新建页相撞。
    expect(parseRoute("/admin/posts/new/edit")).toEqual({
      name: "postEdit",
      slug: "new",
    });

    expect(parseRoute("/admin/pages")).toEqual({ name: "pageList" });
    expect(parseRoute("/admin/pages/new")).toEqual({ name: "pageNew" });
    expect(parseRoute("/admin/pages/about/edit")).toEqual({
      name: "pageEdit",
      slug: "about",
    });
    expect(parseRoute("/admin/pages/new/edit")).toEqual({
      name: "pageEdit",
      slug: "new",
    });

    expect(parseRoute("/admin/users")).toEqual({ name: "userList" });
    expect(parseRoute("/admin/roles")).toEqual({ name: "roleList" });
  });

  it("多余路径段与畸形编码进入 invalid，不静默进入别的页面", () => {
    expect(parseRoute("/admin/posts/new/x")).toEqual({ name: "invalid" });
    expect(parseRoute("/admin/pages/about/edit/x")).toEqual({ name: "invalid" });
    expect(parseRoute("/admin/pages/%")).toEqual({ name: "invalid" });
    expect(parseRoute("/admin/unknown")).toEqual({ name: "invalid" });
    // 用户与角色是固定单段：多余段不静默忽略。
    expect(parseRoute("/admin/users/author")).toEqual({ name: "invalid" });
    expect(parseRoute("/admin/roles/owner")).toEqual({ name: "invalid" });
  });
});
