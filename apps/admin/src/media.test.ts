import { describe, expect, it } from "vitest";
import {
  defaultAltText,
  escapeAltText,
  formatBytes,
  insertImageMarkdown,
  isSupportedImage,
  mediaUrl,
  uploadRejection,
} from "./media";
import { MEDIA_MAX_BYTES } from "./media";

function file(name: string, type: string, size: number): File {
  // jsdom 的 File 以字节数组决定 size；这里用同长度的 Uint8Array 造出目标大小。
  return new File([new Uint8Array(size)], name, { type });
}

describe("媒体客户端预筛", () => {
  it("只接受开放的四类位图 MIME", () => {
    expect(isSupportedImage(file("a.png", "image/png", 1))).toBe(true);
    expect(isSupportedImage(file("a.jpg", "image/jpeg", 1))).toBe(true);
    expect(isSupportedImage(file("a.gif", "image/gif", 1))).toBe(true);
    expect(isSupportedImage(file("a.webp", "image/webp", 1))).toBe(true);
    // SVG 与视频不在第一版范围。
    expect(isSupportedImage(file("a.svg", "image/svg+xml", 1))).toBe(false);
    expect(isSupportedImage(file("a.mp4", "video/mp4", 1))).toBe(false);
    expect(isSupportedImage(file("a.txt", "text/plain", 1))).toBe(false);
    expect(isSupportedImage(file("a.png", "", 1))).toBe(false);
  });

  it("按类型、空文件与大小上限给出可读的拒绝原因", () => {
    expect(uploadRejection(file("a.png", "image/png", 10))).toBeNull();
    expect(uploadRejection(file("a.svg", "image/svg+xml", 10))).toContain("不支持的图片类型");
    expect(uploadRejection(file("a.png", "image/png", 0))).toContain("空文件");
    const tooBig = uploadRejection(file("big.png", "image/png", MEDIA_MAX_BYTES + 1));
    expect(tooBig).toContain("上限");
    // 边界值本身允许通过。
    expect(uploadRejection(file("edge.png", "image/png", MEDIA_MAX_BYTES))).toBeNull();
  });

  it("格式化字节数", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1024)).toBe("1.0 KiB");
    expect(formatBytes(1536)).toBe("1.5 KiB");
    expect(formatBytes(10 * 1024 * 1024)).toBe("10 MiB");
    expect(formatBytes(-1)).toBe("-");
  });

  it("替代文字去掉扩展名并清理分隔符作为默认值", () => {
    expect(defaultAltText("photo.png")).toBe("photo");
    expect(defaultAltText("my_holiday-photo.jpg")).toBe("my holiday photo");
    expect(defaultAltText(".gitignore")).toBe(".gitignore");
  });

  it("媒体地址只此一处构造（与后端 url/cover_url 同形）", () => {
    expect(mediaUrl("media-1")).toBe("/media/media-1");
    // 后端用 UUID；这里不做转义，保持与 `MediaAsset.url` 完全一致。
    expect(mediaUrl("0f8fad5b-d9cb-469f-a165-70867728950e")).toBe(
      "/media/0f8fad5b-d9cb-469f-a165-70867728950e",
    );
  });
});

describe("Markdown 图片插入", () => {
  it("在光标处插入独立块并补足空行", () => {
    const result = insertImageMarkdown("前一段", 3, 3, "/media/x", "图");
    expect(result.value).toBe("前一段\n\n![图](/media/x)");
    // 光标落在插入内容之后，便于继续写正文。
    expect(result.selectionStart).toBe(result.value.length);
    expect(result.selectionEnd).toBe(result.value.length);
  });

  it("已有换行时不重复补空行", () => {
    expect(insertImageMarkdown("前一段\n", 4, 4, "/media/x", "图").value).toBe(
      "前一段\n\n![图](/media/x)",
    );
    expect(insertImageMarkdown("前一段\n\n", 5, 5, "/media/x", "图").value).toBe(
      "前一段\n\n![图](/media/x)",
    );
  });

  it("插入到文中时与前后段落各留一个空行", () => {
    const result = insertImageMarkdown("前\n\n后", 1, 1, "/media/x", "图");
    expect(result.value).toBe("前\n\n![图](/media/x)\n\n后");
  });

  it("替换当前选区而不是插入到选区前", () => {
    const result = insertImageMarkdown("保留这段", 0, 4, "/media/x", "图");
    // "保留"（0..4 覆盖前两个字符的字节区间无意义，这里按字符索引）被替换。
    expect(result.value.startsWith("![图](/media/x)")).toBe(true);
  });

  it("越界选区被夹紧，不会破坏正文", () => {
    const result = insertImageMarkdown("abc", 99, 120, "/media/x", "图");
    expect(result.value).toBe("abc\n\n![图](/media/x)");
    expect(result.selectionStart).toBeLessThanOrEqual(result.value.length);
  });

  it("替代文字里的方括号与换行会被转义", () => {
    expect(escapeAltText("图 [1]\n第二行")).toBe("图 [1\\] 第二行");
    const result = insertImageMarkdown("", 0, 0, "/media/x", "图 ] 说明");
    expect(result.value).toBe("![图 \\] 说明](/media/x)");
  });
});
