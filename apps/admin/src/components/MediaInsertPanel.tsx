import { Alert, Button, Card, Flex, Input, Space, Typography, theme } from "antd";
import { useRef, useState } from "react";
import { MEDIA_ACCEPT, MEDIA_MAX_BYTES, MEDIA_PUBLIC_NOTICE, formatBytes } from "../media";
import type { ImageInsertion } from "./useImageInsertion";
import { MediaBrowser } from "./MediaBrowser";

/** 图片选择只修改本地正文；面板、拖入和粘贴共享编辑器持有的插入位置与上传状态。 */
export function MediaInsertPanel({ insertion, canUpload, onClose }: {
  insertion: ImageInsertion;
  canUpload: boolean;
  onClose: () => void;
}) {
  const { token } = theme.useToken();
  const [alt, setAlt] = useState("");
  const [dragActive, setDragActive] = useState(false);
  const fileInput = useRef<HTMLInputElement | null>(null);

  return <Card size="small" title="插入图片" aria-label="插入图片"
    onDragOver={event => { event.preventDefault(); setDragActive(canUpload); }}
    onDragLeave={() => setDragActive(false)}
    onDrop={event => {
      event.preventDefault(); setDragActive(false);
      if (canUpload) void insertion.insertFiles(Array.from(event.dataTransfer.files));
    }}
    style={dragActive ? { borderColor: token.colorPrimary } : undefined}
    extra={<Space wrap>
      <Typography.Text type="secondary">单张上限 {formatBytes(MEDIA_MAX_BYTES)}，仅 PNG/JPEG/GIF/WebP</Typography.Text>
      {canUpload && <>
        <input ref={fileInput} type="file" accept={MEDIA_ACCEPT} multiple hidden
          onChange={event => {
            const files = Array.from(event.target.files ?? []);
            event.target.value = "";
            void insertion.insertFiles(files);
          }} />
        <Button disabled={insertion.busy} onClick={() => fileInput.current?.click()}>
          {insertion.busy ? "上传中…" : "上传图片"}
        </Button>
      </>}
      <Button type="link" onClick={onClose}>收起</Button>
    </Space>}>
    <Flex vertical gap={12}>
      {canUpload && <Typography.Text type="secondary">{MEDIA_PUBLIC_NOTICE}</Typography.Text>}
      <Flex vertical gap={4}>
        <label htmlFor="media-insert-alt">替代文字</label>
        <Input id="media-insert-alt" value={alt} onChange={event => setAlt(event.target.value)}
          placeholder="描述图片内容；留空则用文件名" />
      </Flex>
      {insertion.error !== null && <Alert type="error" showIcon title={insertion.error} />}
      {insertion.notice !== null && <Alert type="success" showIcon title={insertion.notice} />}
      <MediaBrowser actionLabel="插入" disabled={insertion.busy}
        onChoose={asset => insertion.insertAsset(asset, alt.trim() ? alt : asset.original_name)}
        emptyDescription={canUpload ? "媒体库还是空的：拖入、粘贴或点击「上传图片」。" : "媒体库还是空的。"} />
      {canUpload && <Typography.Text type="secondary">也可以直接把图片拖到正文框，或在正文框内粘贴剪贴板图片。</Typography.Text>}
    </Flex>
  </Card>;
}
