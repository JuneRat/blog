/**
 * 媒体相关的纯函数与常量。
 *
 * 有意不触碰网络、弹窗与 React：上传前的客户端校验、字节格式化与
 * 「在光标处插入 Markdown」都能被单测直接驱动。
 */

/** 允许上传的位图 MIME（与后端 `domain::media::ImageFormat` 一致）。 */
export const MEDIA_ACCEPT = "image/png,image/jpeg,image/gif,image/webp";

/**
 * 站内媒体地址的唯一构造处。
 *
 * 后端下发的 `cover_url` 与资产条目的 `url` 都是这个形状；封面选择器在只有
 * id（例如刚选中、或后端未回传地址）时也要能显示缩略图，所以形状只在这里写一次，
 * 避免 `"/media/" + id` 散落到各屏后与后端不一致。
 */
export function mediaUrl(id: string): string {
  return `/media/${id}`;
}

/** 单张图片上限（与后端 `MAX_IMAGE_BYTES` 一致）。 */
export const MEDIA_MAX_BYTES = 10 * 1024 * 1024;

const SUPPORTED_MIME = new Set(MEDIA_ACCEPT.split(","));

/**
 * 按 MIME 预筛文件类型。
 *
 * 真正的判据是**文件内容**（服务端按文件头嗅探），这里只用来提前给出
 * 「这个类型不支持」的友好提示，避免把 SVG、视频或任意附件白跑一趟网络。
 */
export function isSupportedImage(file: File): boolean {
  return SUPPORTED_MIME.has(file.type);
}

/** 人类可读的字节数。 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "-";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KiB", "MiB", "GiB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1) : String(Math.round(value))} ${units[unit]}`;
}

/** 上传前的客户端校验：返回错误文案，`null` 表示可以发送。 */
export function uploadRejection(file: File): string | null {
  if (!isSupportedImage(file)) {
    return `不支持的图片类型：${file.type || "未知"}。仅支持 PNG、JPEG、GIF、WebP。`;
  }
  if (file.size === 0) return `${file.name} 是空文件。`;
  if (file.size > MEDIA_MAX_BYTES) {
    return `${file.name} 为 ${formatBytes(file.size)}，超过 ${formatBytes(MEDIA_MAX_BYTES)} 上限。`;
  }
  return null;
}

/**
 * 转义 Markdown 图片替代文字。
 *
 * `]` 会提前闭合图片语法，换行会破坏所在段落：两者都必须先处理，
 * 否则用户输入的替代文字会静默改变正文结构。
 */
export function escapeAltText(alt: string): string {
  return alt.replace(/[\r\n]+/g, " ").replace(/\\/g, "\\\\").replace(/]/g, "\\]");
}

/** 插入结果：新值与新的选区（React 侧据此回写 state 与 DOM）。 */
export interface InsertResult {
  value: string;
  selectionStart: number;
  selectionEnd: number;
}

/** 判断某个位置是否已经处于「独立块」的边界。 */
function blockPadding(before: string, after: string): { prefix: string; suffix: string } {
  const prefix = before.length === 0 || before.endsWith("\n\n") ? "" : before.endsWith("\n") ? "\n" : "\n\n";
  const suffix = after.length === 0 || after.startsWith("\n") ? "" : "\n\n";
  return { prefix, suffix };
}

/**
 * 在光标处插入站内图片 Markdown。
 *
 * - 自动补空行，让图片成为独立块（Markdown 里与文字同段会被渲染成行内图片）；
 * - 替换当前选区（拖入/粘贴时用户往往已选好位置）；
 * - 返回的光标落在插入内容之后，用户可以立刻继续写正文。
 */
export function insertImageMarkdown(
  value: string,
  start: number,
  end: number,
  url: string,
  alt: string,
): InsertResult {
  const safeStart = Math.max(0, Math.min(start, value.length));
  const safeEnd = Math.max(safeStart, Math.min(end, value.length));
  const before = value.slice(0, safeStart);
  const after = value.slice(safeEnd);
  const { prefix, suffix } = blockPadding(before, after);
  const syntax = `![${escapeAltText(alt)}](${url})`;
  const inserted = `${prefix}${syntax}${suffix}`;
  return {
    value: `${before}${inserted}${after}`,
    selectionStart: safeStart + prefix.length + syntax.length,
    selectionEnd: safeStart + prefix.length + syntax.length,
  };
}

/** 默认替代文字：用文件名去扩展名，避免把 `photo.png` 原样读给屏幕阅读器。 */
export function defaultAltText(fileName: string): string {
  const base = fileName.replace(/\.[^.]+$/, "").replace(/[_-]+/g, " ").trim();
  return base.length > 0 ? base : fileName;
}
