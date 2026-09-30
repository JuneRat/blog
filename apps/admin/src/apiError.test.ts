import { describe, expect, it } from "vitest";
import { ApiError } from "./api/client";
import { messageOf, permissionMessageOf } from "./apiError";

/**
 * 两个入口的差别就是本模块存在的理由：管理界面要给 403 加「没有权限：」前缀，
 * 登录与角色目录不能加。谁改成一个入口，这里会红。
 */
describe("错误文案", () => {
  it("ApiError 带上请求编号", () => {
    expect(messageOf(new ApiError(409, "版本冲突", "version_conflict", "req-1"))).toBe(
      "版本冲突（错误编号 req-1）",
    );
  });

  it("没有请求编号时不加括号", () => {
    expect(messageOf(new ApiError(500, "服务异常", "internal", null))).toBe("服务异常");
  });

  it("messageOf 不给 403 加权限前缀（登录失败不是权限问题）", () => {
    expect(messageOf(new ApiError(403, "无权执行该操作", "forbidden", "req-2"))).toBe(
      "无权执行该操作（错误编号 req-2）",
    );
  });

  it("permissionMessageOf 只给 403 加「没有权限：」", () => {
    expect(permissionMessageOf(new ApiError(403, "无权查看", "forbidden", "req-3"))).toBe(
      "没有权限：无权查看（错误编号 req-3）",
    );
    expect(permissionMessageOf(new ApiError(409, "版本冲突", "version_conflict", "req-4"))).toBe(
      "版本冲突（错误编号 req-4）",
    );
  });

  it("非 ApiError：Error 用 message，其余归为未知错误", () => {
    expect(messageOf(new Error("断网了"))).toBe("断网了");
    expect(permissionMessageOf(null)).toBe("未知错误");
    expect(permissionMessageOf("字符串不是错误")).toBe("未知错误");
  });
});
