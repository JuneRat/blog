import { invalidateAfterWrite } from "../queryEffects";
import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import type { RefObject } from "react";
import { mediaApi } from "../api";
import { permissionMessageOf } from "../apiError";
import type { MediaAsset } from "../types";
import { defaultAltText, insertImageMarkdown, uploadRejection } from "../media";
import { useEditorRequestGuard } from "../useEditorRequestGuard";

/**
 * 上传一组图片，按顺序返回。
 *
 * 顺序上传而不是并发：一次插入多张时正文里的顺序应与用户选择顺序一致，
 * 并发返回顺序不确定；图片数量很小，串行延迟可以接受。
 * 客户端预筛只拦明显不支持的输入，真正的判定在服务端（按文件内容）。
 */
export async function uploadImages(files: File[], onUploaded: () => void): Promise<MediaAsset[]> {
  const uploaded: MediaAsset[] = [];
  for (const file of files) {
    const rejection = uploadRejection(file);
    if (rejection !== null) throw new Error(rejection);
    uploaded.push(await mediaApi.upload(file));
    onUploaded();
  }
  return uploaded;
}

export interface ImageInsertion {
  /** 同一编辑目标的封面上传共用此标识；新稿获得 ID 时保持不变。 */
  uploadScope: string;
  busy: boolean;
  error: string | null;
  notice: string | null;
  clear: () => void;
  /** 插入一张已存在于媒体库的图片。 */
  insertAsset: (asset: MediaAsset, alt: string) => void;
  /** 上传若干文件并插入（拖入与粘贴共用）。 */
  insertFiles: (files: File[]) => Promise<void>;
}

/**
 * 编辑器内的图片插入行为。
 *
 * 插入位置取自 textarea 的真实选区（不依赖 React state），插入后把光标
 * 放到内容之后。文章与页面编辑器共用同一实现，避免两处各写一套偏移计算。
 *
 * 注意：**插入只改本地表单**，不会自动保存——已发布内容的行为仍是
 * 「明确点击保存并更新线上」，与既有编辑语义一致。
 */
export function useImageInsertion(
  contentRef: RefObject<HTMLTextAreaElement | null>,
  readContent: () => string,
  commitContent: (next: string) => void,
  editor: { id: string | null; loadedId: string | null },
): ImageInsertion {
  const queryClient = useQueryClient();
  const target = useRef({ id: editor.id, generation: 0 });
  if (target.current.id !== editor.id) {
    // 新稿保存后获得 ID 仍是同一份正文，上传应继续跟随保存期间的新输入。
    const created = target.current.id === null && editor.id === editor.loadedId;
    target.current = { id: editor.id, generation: target.current.generation + (created ? 0 : 1) };
  }
  const generation = target.current.generation;
  const beginRequest = useEditorRequestGuard(String(generation));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  /** 插入后要恢复的选区；DOM 更新完成后由 `restore` 应用。 */
  const pendingSelection = useRef<{ start: number; end: number } | null>(null);
  /**
   * 同步闸门：`busy` 要等下一次渲染才为真，同一 tick 内的两批拖放/粘贴
   * 都会看到 `busy === false`，于是并发上传并各自按「当前选区」插入，
   * 后一批的光标位置会与先一批竞争。闸门必须在调用时就生效。
   */
  const inFlight = useRef(false);

  useEffect(() => {
    inFlight.current = false;
    pendingSelection.current = null;
    setBusy(false);
    setError(null);
    setNotice(null);
  }, [generation]);

  /** 选区：textarea 不存在或尚未挂载时退化为「正文末尾」。 */
  const selection = useCallback((): { start: number; end: number } => {
    const value = readContent();
    const element = contentRef.current;
    if (element === null) return { start: value.length, end: value.length };
    return {
      start: Math.min(element.selectionStart ?? value.length, value.length),
      end: Math.min(element.selectionEnd ?? value.length, value.length),
    };
  }, [contentRef, readContent]);

  const restore = useCallback((): void => {
    const pending = pendingSelection.current;
    if (pending === null) return;
    pendingSelection.current = null;
    const element = contentRef.current;
    if (element === null) return;
    element.focus();
    element.setSelectionRange(pending.start, pending.end);
  }, [contentRef]);

  const apply = useCallback(
    (value: string, start: number, end: number): void => {
      commitContent(value);
      pendingSelection.current = { start, end };
      // React 提交后 DOM 才是新值；下一帧恢复选区。测试环境可能没有 rAF。
      if (typeof requestAnimationFrame === "function") {
        requestAnimationFrame(restore);
      } else {
        restore();
      }
    },
    [commitContent, restore],
  );

  const insertAsset = useCallback(
    (asset: MediaAsset, alt: string): void => {
      const { start, end } = selection();
      const result = insertImageMarkdown(readContent(), start, end, asset.url, alt);
      apply(result.value, result.selectionStart, result.selectionEnd);
      setError(null);
      setNotice("已插入正文（尚未保存，点保存后生效）。");
    },
    [apply, readContent, selection],
  );

  const insertFiles = useCallback(
    async (files: File[]): Promise<void> => {
      if (files.length === 0 || inFlight.current) return;
      const isCurrent = beginRequest();
      inFlight.current = true;
      setError(null);
      setNotice(null);
      setBusy(true);
      try {
        const uploaded = await uploadImages(files, () => { void invalidateAfterWrite(queryClient, "media"); });
        if (!isCurrent()) return;
        // 多张图片按选择顺序依次插入，后一张接在前一张之后。
        let value = readContent();
        let caret = selection().start;
        for (const asset of uploaded) {
          const result = insertImageMarkdown(
            value,
            caret,
            caret,
            asset.url,
            defaultAltText(asset.original_name),
          );
          value = result.value;
          caret = result.selectionStart;
        }
        apply(value, caret, caret);
        setNotice(`已插入 ${uploaded.length} 张图片（尚未保存，点保存后生效）。`);
      } catch (e) {
        if (isCurrent()) setError(permissionMessageOf(e));
      } finally {
        if (isCurrent()) {
          inFlight.current = false;
          setBusy(false);
        }
      }
    },
    [apply, beginRequest, queryClient, readContent, selection],
  );

  const clear = useCallback((): void => {
    setError(null);
    setNotice(null);
  }, []);

  return { uploadScope: String(generation), busy, error, notice, clear, insertAsset, insertFiles };
}
