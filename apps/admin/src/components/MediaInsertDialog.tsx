import { Alert, Button, Flex, Input, Modal, Typography, theme } from "antd";
import { useRef, useState } from "react";
import { MEDIA_ACCEPT, MEDIA_MAX_BYTES, MEDIA_PUBLIC_NOTICE, formatBytes } from "../media";
import type { ImageInsertion } from "./useImageInsertion";
import { MediaBrowser } from "./MediaBrowser";

/** 上传和媒体库共用一个弹窗，成功插入后关闭；选区仍由编辑器管理。 */
export function MediaInsertDialog({ insertion, canRead, canUpload, onClose }: {
  insertion: ImageInsertion;
  canRead: boolean;
  canUpload: boolean;
  onClose: () => void;
}) {
  const { token } = theme.useToken();
  const [alt, setAlt] = useState("");
  const [dragActive, setDragActive] = useState(false);
  const fileInput = useRef<HTMLInputElement | null>(null);

  async function upload(files: File[]): Promise<void> {
    if (canUpload && await insertion.insertFiles(files)) onClose();
  }

  return <Modal title="插入图片" open centered onCancel={onClose} footer={null} width={760}
    closable={{ "aria-label": "关闭插图窗口" }}
    styles={{ body: { maxHeight: "70vh", overflowY: "auto" } }}>
    <Flex vertical gap={12}
      onDragOver={event => { event.preventDefault(); setDragActive(canUpload); }}
      onDragLeave={() => setDragActive(false)}
      onDrop={event => {
        event.preventDefault(); setDragActive(false);
        void upload(Array.from(event.dataTransfer.files));
      }}
      style={dragActive ? { outline: `2px dashed ${token.colorPrimary}`, outlineOffset: -2 } : undefined}>
      {canUpload && <>
        <Flex align="center" gap={12} wrap>
          <input ref={fileInput} type="file" aria-label="选择要上传的图片" accept={MEDIA_ACCEPT} multiple hidden disabled={insertion.busy}
            onChange={event => {
              const files = Array.from(event.target.files ?? []);
              event.target.value = "";
              void upload(files);
            }} />
          <Button type="primary" disabled={insertion.busy} onClick={() => fileInput.current?.click()}>
            {insertion.busy ? "上传中…" : "上传图片"}
          </Button>
          <Typography.Text type="secondary">单张上限 {formatBytes(MEDIA_MAX_BYTES)}，仅 PNG/JPEG/GIF/WebP</Typography.Text>
        </Flex>
        <Typography.Text type="secondary">{MEDIA_PUBLIC_NOTICE}</Typography.Text>
      </>}
      {insertion.error !== null && <Alert type="error" showIcon title={insertion.error} />}
      {canRead && <>
        <Typography.Text strong>从媒体库选择</Typography.Text>
        <Flex vertical gap={4}>
          <label htmlFor="media-insert-alt">替代文字</label>
          <Input id="media-insert-alt" value={alt} onChange={event => setAlt(event.target.value)}
            placeholder="描述图片内容；留空则用文件名" />
        </Flex>
        <MediaBrowser actionLabel="插入" disabled={insertion.busy}
          onChoose={asset => { insertion.insertAsset(asset, alt.trim() ? alt : asset.original_name); onClose(); }}
          emptyDescription={canUpload ? "媒体库还是空的：点击「上传图片」或拖入图片。" : "媒体库还是空的。"} />
      </>}
      {canUpload && <Typography.Text type="secondary">也可以直接把图片拖到正文框，或在正文框内粘贴剪贴板图片。</Typography.Text>}
    </Flex>
  </Modal>;
}
