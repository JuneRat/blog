import { useCallback, useEffect, useRef, useState } from "react";
import { mediaApi } from "../api";
import type { MediaAsset } from "../types";
import { MEDIA_ACCEPT, formatBytes } from "../media";
import { messageOf } from "./useImageInsertion";
import type { ImageInsertion } from "./useImageInsertion";

/** 面板一次展示的资产数（第一页就够用，必要时可去媒体库查找）。 */
const PANEL_PAGE_SIZE = 24;

/**
 * 编辑器内的图片面板：上传、浏览最近上传的图片、填写替代文字后插入光标位置。
 *
 * 面板不直接接触 textarea：插入状态与位置由编辑器持有的 `useImageInsertion`
 * 提供（同一个实例也服务拖入与粘贴），因此面板插入、拖入与粘贴共享同一路径，
 * 也不会出现两份互相覆盖的提示与 busy 状态。
 */
export function MediaInsertPanel({
  insertion,
  canUpload,
  onClose,
}: {
  insertion: ImageInsertion;
  /** 没有 media.upload 时不展示上传入口（后端同样会拒绝，这里只是不给死路）。 */
  canUpload: boolean;
  onClose: () => void;
}) {
  const [assets, setAssets] = useState<MediaAsset[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [alt, setAlt] = useState("");
  const [dragActive, setDragActive] = useState(false);
  const fileInput = useRef<HTMLInputElement | null>(null);

  const load = useCallback(async () => {
    setLoadError(null);
    try {
      const page = await mediaApi.list(1);
      setAssets(page.items);
    } catch (e) {
      setAssets([]);
      setLoadError(messageOf(e));
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /** 上传：面板内选择文件，成功后直接插入光标处并刷新列表。 */
  async function upload(files: File[]): Promise<void> {
    await insertion.insertFiles(files);
    await load();
  }

  return (
    <section
      className={`media-panel${dragActive ? " drag-active" : ""}`}
      aria-label="插入图片"
      onDragOver={(event) => {
        event.preventDefault();
        setDragActive(true);
      }}
      onDragLeave={() => setDragActive(false)}
      onDrop={(event) => {
        event.preventDefault();
        setDragActive(false);
        if (canUpload) void upload(Array.from(event.dataTransfer.files));
      }}
    >
      <header className="media-panel-head">
        <strong>插入图片</strong>
        <span className="muted">
          单张上限 {formatBytes(10 * 1024 * 1024)}，仅 PNG/JPEG/GIF/WebP
        </span>
        <input
          ref={fileInput}
          type="file"
          accept={MEDIA_ACCEPT}
          multiple
          hidden
          onChange={(event) => {
            const files = Array.from(event.target.files ?? []);
            event.target.value = "";
            void upload(files);
          }}
        />
        {canUpload && (
          <button
            type="button"
            className="button ghost"
            disabled={insertion.busy}
            onClick={() => fileInput.current?.click()}
          >
            {insertion.busy ? "上传中…" : "上传图片"}
          </button>
        )}
        <button type="button" className="link" onClick={onClose}>
          收起
        </button>
      </header>

      <label className="media-alt">
        替代文字
        <input
          value={alt}
          onChange={(event) => setAlt(event.target.value)}
          placeholder="描述图片内容；留空则用文件名"
        />
      </label>

      {insertion.error !== null && <p className="error">{insertion.error}</p>}
      {insertion.notice !== null && <p className="notice">{insertion.notice}</p>}
      {loadError !== null && <p className="error">{loadError}</p>}
      {assets === null && <p className="muted">正在加载媒体库…</p>}
      {assets !== null && assets.length === 0 && loadError === null && (
        <p className="muted">媒体库还是空的：拖入、粘贴或点击「上传图片」。</p>
      )}

      {assets !== null && assets.length > 0 && (
        <ul className="media-grid">
          {assets.slice(0, PANEL_PAGE_SIZE).map((asset) => (
            <li key={asset.id} className="media-card">
              <img src={asset.url} alt={asset.original_name} loading="lazy" />
              <div className="media-meta">
                <code title={asset.original_name}>{asset.original_name}</code>
                <span className="muted">
                  {asset.width}×{asset.height} · {formatBytes(asset.byte_size)}
                </span>
              </div>
              <button
                type="button"
                className="button ghost"
                disabled={insertion.busy}
                onClick={() =>
                  insertion.insertAsset(asset, alt.trim().length > 0 ? alt : asset.original_name)
                }
              >
                插入
              </button>
            </li>
          ))}
        </ul>
      )}

      <p className="muted">
        提示：也可以直接把图片拖到正文框，或在正文框内粘贴剪贴板图片。
      </p>
    </section>
  );
}
