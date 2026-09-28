import { describe, expect, it } from "vitest";
import { dateTimeInput, formatDateTime, inputToInstant } from "./timeZone";

describe("站点时区", () => {
  it("转换跨日的显示和预约，保留同一 UTC 时刻", () => {
    const instant = "2026-09-28T17:30:00.000Z";
    expect(dateTimeInput(instant, "Asia/Shanghai")).toBe("2026-09-29T01:30");
    expect(inputToInstant("2026-09-29T01:30", "Asia/Shanghai")).toBe(instant);
    expect(formatDateTime(instant, "Asia/Shanghai")).toBe("2026-09-29 01:30:00 +08:00 (Asia/Shanghai)");
    expect(dateTimeInput(instant, "UTC")).toBe("2026-09-28T17:30");
  });

  it("应用夏令时规则并拒绝不唯一的墙上时间", () => {
    const zone = "America/New_York";
    expect(inputToInstant("2026-01-15T07:00", zone)).toBe("2026-01-15T12:00:00.000Z");
    expect(inputToInstant("2026-07-15T08:00", zone)).toBe("2026-07-15T12:00:00.000Z");
    expect(inputToInstant("2026-03-08T02:30", zone)).toBeNull(); // gap
    expect(inputToInstant("2026-11-01T01:30", zone)).toBeNull(); // fold
  });

  it("拒绝无效日期、时区和带偏移的本地输入", () => {
    expect(inputToInstant("2026-02-30T12:00", "Asia/Shanghai")).toBeNull();
    expect(inputToInstant("2026-09-29T01:30", "Asia/Unknown")).toBeNull();
    expect(inputToInstant("2026-09-29T01:30Z", "Asia/Shanghai")).toBeNull();
    expect(inputToInstant("", "UTC")).toBeNull();
    expect(dateTimeInput(null, "UTC")).toBe("");
  });
});
