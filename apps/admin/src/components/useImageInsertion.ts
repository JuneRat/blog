import { invalidateAfterWrite } from "../queryEffects";
import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import type { RefObject } from "react";
import type { MarkdownEditorHandle, EditorImage } from "./editorHandle";
import { mediaApi } from "../api/media";
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
  /** 上传若干文件并插入（拖入与粘贴共用），成功插入当前正文时返回 true。 */
  insertFiles: (files: File[]) => Promise<boolean>;
}

/**
 * 编辑器内的图片插入行为。
 *
 * 插入通过当前编辑模式的选区适配器完成，源码和即时渲染共用上传与过期响应保护。
 *
 * 注意：**插入只改本地表单**，不会自动保存——已发布内容的行为仍是
 * 「明确点击保存并更新线上」，与既有编辑语义一致。
 */
export function useImageInsertion(
  contentRef: RefObject<MarkdownEditorHandle | null>,
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
  /**
   * 同步闸门：`busy` 要等下一次渲染才为真，同一 tick 内的两批拖放/粘贴
   * 都会看到 `busy === false`，于是并发上传并各自按「当前选区」插入，
   * 后一批的光标位置会与先一批竞争。闸门必须在调用时就生效。
   */
  const inFlight = useRef(false);

  useEffect(() => {
    inFlight.current = false;
    setBusy(false);
    setError(null);
    setNotice(null);
  }, [generation]);

  const apply = useCallback((images: EditorImage[], replaceSelection: boolean) => {
    if (contentRef.current) {
      commitContent(contentRef.current.insertImages(images, replaceSelection));
    } else {
      let value = readContent();
      for (const image of images) value = insertImageMarkdown(value, value.length, value.length, image.url, image.alt).value;
      commitContent(value);
    }
  }, [contentRef, commitContent, readContent]);

  const insertAsset = useCallback(
    (asset: MediaAsset, alt: string): void => {
      apply([{ url: asset.url, alt }], true);
      setError(null);
      setNotice("已插入正文（尚未保存，点保存后生效）。");
    },
    [apply],
  );

  const insertFiles = useCallback(
    async (files: File[]): Promise<boolean> => {
      if (files.length === 0 || inFlight.current) return false;
      const isCurrent = beginRequest();
      inFlight.current = true;
      setError(null);
      setNotice(null);
      setBusy(true);
      try {
        const uploaded = await uploadImages(files, () => { void invalidateAfterWrite(queryClient, "media"); });
        if (!isCurrent()) return false;
        apply(uploaded.map(asset => ({ url: asset.url, alt: defaultAltText(asset.original_name) })), false);
        setNotice(`已插入 ${uploaded.length} 张图片（尚未保存，点保存后生效）。`);
        return true;
      } catch (e) {
        if (isCurrent()) setError(permissionMessageOf(e));
        return false;
      } finally {
        if (isCurrent()) {
          inFlight.current = false;
          setBusy(false);
        }
      }
    },
    [apply, beginRequest, queryClient],
  );

  const clear = useCallback((): void => {
    setError(null);
    setNotice(null);
  }, []);

  return { uploadScope: String(generation), busy, error, notice, clear, insertAsset, insertFiles };
}
