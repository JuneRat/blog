import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, mediaApi } from "../api";
import { useAuth } from "../auth";
import { navigate, paths } from "../router";
import type { MediaAsset, MediaPage, MediaUsageView } from "../types";
import { MEDIA_ACCEPT, formatBytes } from "../media";
import { messageOf, uploadImages } from "../components/useImageInsertion";

/**
 * 媒体库屏（`/admin/media`）。
 *
 * - 按上传时间分页，显示缩略图、文件名、大小、尺寸、上传者与引用状态；
 * - 可复制站内地址，或在编辑器中「插入图片」面板里选择；
 * - **未被任何内容引用**才可删除；仍被引用时后端 409 `media_in_use`，
 *   这里随后拉取使用位置并逐条列出（草稿/私密/回收站引用同样占用）。
 *
 * 权限由后端判定：无 `media.read` 时列表本身就会 403，界面只做入口隐藏。
 */
export function MediaLibraryScreen() {
  const { me } = useAuth();
  const [page, setPage] = useState(1);
  const [data, setData] = useState<MediaPage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /** 删除被拒后展示的使用位置（含「为什么不能删」的全部来源）。 */
  const [usage, setUsage] = useState<MediaUsageView | null>(null);
  const [dragActive, setDragActive] = useState(false);
  const fileInput = useRef<HTMLInputElement | null>(null);

  const canUpload = me?.permissions.includes("media.upload") ?? false;
  const canDeleteAny = me?.permissions.includes("media.delete_any") ?? false;
  const canDeleteOwn = me?.permissions.includes("media.delete") ?? false;
  const viewerName = me?.user_id ?? null;

  const load = useCallback(async (target: number) => {
    setError(null);
    try {
      setData(await mediaApi.list(target));
    } catch (e) {
      setData({ items: [], total: 0, page: target, per_page: 0 });
      setError(messageOf(e));
    }
  }, []);

  useEffect(() => {
    void load(page);
  }, [load, page]);

  async function upload(files: File[]): Promise<void> {
    if (files.length === 0) return;
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      const uploaded = await uploadImages(files);
      setNotice(`已上传 ${uploaded.length} 张图片；默认可被你自己引用，公开后匿名才可读取。`);
      setPage(1);
      await load(1);
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  function canDelete(asset: MediaAsset): boolean {
    if (asset.reference_count > 0) return false;
    if (canDeleteAny) return true;
    return canDeleteOwn && viewerName !== null && asset.owner_id === viewerName;
  }

  async function remove(asset: MediaAsset): Promise<void> {
    if (!window.confirm(`删除「${asset.original_name}」？文件会被移除且不可恢复。`)) return;
    setError(null);
    setNotice(null);
    setUsage(null);
    setBusy(true);
    try {
      await mediaApi.remove(asset.id, asset.version);
      setNotice(`已删除 ${asset.original_name}。`);
      await load(page);
    } catch (e) {
      setError(messageOf(e));
      // 引用保护被触发时把使用位置摊开，让用户知道该去哪里解除引用。
      if (e instanceof ApiError && e.code === "media_in_use") {
        try {
          setUsage(await mediaApi.detail(asset.id));
        } catch {
          // 使用位置读取失败不影响主错误提示。
        }
      }
    } finally {
      setBusy(false);
    }
  }

  async function copy(asset: MediaAsset): Promise<void> {
    const url = new URL(asset.url, window.location.origin).toString();
    try {
      await navigator.clipboard.writeText(url);
      setNotice(`已复制地址：${asset.url}`);
    } catch {
      // 无剪贴板权限（或非安全上下文）时退化为展示，绝不假装成功。
      setNotice(`复制失败，请手动复制：${asset.url}`);
    }
  }

  const totalPages = data === null ? 0 : Math.max(1, Math.ceil(data.total / (data.per_page || 1)));

  return (
    <div className="screen">
      <header className="topbar">
        <div>
          <button type="button" className="link" onClick={() => navigate(paths.list)}>
            ← 返回文章
          </button>
          <h1>媒体库</h1>
        </div>
        <div className="topbar-actions">
          {data !== null && <span className="muted">共 {data.total} 张</span>}
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
              className="button"
              disabled={busy}
              onClick={() => fileInput.current?.click()}
            >
              {busy ? "处理中…" : "上传图片"}
            </button>
          )}
        </div>
      </header>

      {error !== null && <p className="error">{error}</p>}
      {notice !== null && <p className="notice">{notice}</p>}

      {usage !== null && (
        <div className="conflict">
          <strong>「{usage.media.original_name}」仍被以下内容引用，无法删除：</strong>
          <ul className="usage-list">
            {usage.references.map((reference) => (
              <li key={`${reference.kind}-${reference.content_id}`}>
                <button
                  type="button"
                  className="link"
                  onClick={() =>
                    navigate(
                      reference.kind === "post"
                        ? paths.editPost(reference.slug)
                        : paths.editPage(reference.slug),
                    )
                  }
                >
                  {reference.kind === "post" ? "文章" : "页面"}：{reference.title || reference.slug}
                </button>
                <span className="muted">
                  {" "}
                  {reference.deleted
                    ? "回收站"
                    : reference.status === "published"
                      ? "已发布"
                      : "草稿"}
                  {reference.public ? "；公开可读" : "；不公开"}
                </span>
              </li>
            ))}
          </ul>
          {usage.hidden_references > 0 && (
            <p className="muted">
              另有 {usage.hidden_references} 处引用你无权查看（他人草稿、私密内容，
              或你没有页面读取权限的内容）。删除保护按全部引用判定，
              因此仍需对方解除引用。
            </p>
          )}
          <p className="muted">在上面的内容里移除该图片并保存后，才能删除。</p>
        </div>
      )}

      {canUpload && (
        <div
          className={`media-dropzone${dragActive ? " drag-active" : ""}`}
          onDragOver={(event) => {
            event.preventDefault();
            setDragActive(true);
          }}
          onDragLeave={() => setDragActive(false)}
          onDrop={(event) => {
            event.preventDefault();
            setDragActive(false);
            void upload(Array.from(event.dataTransfer.files));
          }}
        >
          把图片拖到这里上传；也可以在文章或页面编辑器里直接拖入、粘贴。
        </div>
      )}

      {data === null && <p className="muted">正在加载…</p>}
      {data !== null && data.items.length === 0 && error === null && (
        <p className="muted">媒体库还是空的。</p>
      )}

      {data !== null && data.items.length > 0 && (
        <>
          <ul className="media-grid wide">
            {data.items.map((asset) => (
              <li key={asset.id} className="media-card">
                <img src={asset.url} alt={asset.original_name} loading="lazy" />
                <div className="media-meta">
                  <code title={asset.original_name}>{asset.original_name}</code>
                  <span className="muted">
                    {asset.width}×{asset.height} · {formatBytes(asset.byte_size)} ·{" "}
                    {asset.owner_display}
                  </span>
                  <span className="muted">
                    {asset.reference_count === 0
                      ? "未被引用"
                      : `被 ${asset.reference_count} 处引用`}
                    {asset.public_reference_count > 0
                      ? `（${asset.public_reference_count} 处公开可读）`
                      : "（不公开）"}
                  </span>
                  <span className="muted">{asset.created_at}</span>
                </div>
                <div className="media-actions">
                  <button type="button" className="button ghost" onClick={() => void copy(asset)}>
                    复制地址
                  </button>
                  <a className="link" href={asset.url} target="_blank" rel="noreferrer">
                    预览
                  </a>
                  <button
                    type="button"
                    className="button danger"
                    disabled={busy || !canDelete(asset)}
                    title={
                      asset.reference_count > 0
                        ? "仍被内容引用，先移除引用"
                        : canDelete(asset)
                          ? "删除图片"
                          : "需要 media.delete（本人上传）或 media.delete_any"
                    }
                    onClick={() => void remove(asset)}
                  >
                    删除
                  </button>
                </div>
              </li>
            ))}
          </ul>
          {totalPages > 1 && (
            <div className="pager">
              <button
                type="button"
                className="button ghost"
                disabled={page <= 1}
                onClick={() => setPage(page - 1)}
              >
                上一页
              </button>
              <span className="muted">
                第 {page} / {totalPages} 页
              </span>
              <button
                type="button"
                className="button ghost"
                disabled={page >= totalPages}
                onClick={() => setPage(page + 1)}
              >
                下一页
              </button>
            </div>
          )}
        </>
      )}
    </div>
  );
}
