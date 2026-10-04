import { describe, expect, it } from "vitest";
import { codePointLength } from "./text";

describe("codePointLength", () => {
  it("按码点计数，不受 UTF-16 代理对所影响", () => {
    expect(codePointLength("")).toBe(0);
    expect(codePointLength("abc")).toBe(3);
    expect(codePointLength("中文标题")).toBe(4);
    // 增补平面字符：UTF-16 长度是 2，码点数才是后端使用的口径。
    expect("😀".length).toBe(2);
    expect(codePointLength("😀")).toBe(1);
    expect(codePointLength("😀".repeat(200))).toBe(200);
  });

  it("与 String.length 的差异只出现在增补平面字符上", () => {
    expect(codePointLength("a😀b")).toBe(3);
    expect("a😀b".length).toBe(4);
  });
});
