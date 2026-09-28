import {
  Alert,
  Button,
  Card,
  Empty,
  Flex,
  Image,
  Input,
  Space,
  Spin,
  Typography,
  theme,
} from "antd";
import { useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { mediaPageQuery } from "../mediaQueries";
import { MEDIA_ACCEPT, formatBytes } from "../media";
import { permissionMessageOf } from "../apiError";
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
  const { token } = theme.useToken();
  const [alt, setAlt] = useState("");
  const [dragActive, setDragActive] = useState(false);
  const fileInput = useRef<HTMLInputElement | null>(null);

  const query = useQuery({ ...mediaPageQuery(1), staleTime: 0 });
  const assets = query.data?.items ?? (query.isError ? [] : null);
  const loadError = query.error ? permissionMessageOf(query.error) : null;

  /** 上传：面板内选择文件，成功后直接插入光标处并刷新列表。 */
  async function upload(files: File[]): Promise<void> {
    await insertion.insertFiles(files);
  }

  return (
    <Card
      size="small"
      title="插入图片"
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
      style={dragActive ? { borderColor: token.colorPrimary } : undefined}
      extra={
        <Space>
          <Typography.Text type="secondary">
            单张上限 {formatBytes(10 * 1024 * 1024)}，仅 PNG/JPEG/GIF/WebP
          </Typography.Text>
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
            <Button disabled={insertion.busy} onClick={() => fileInput.current?.click()}>
              {insertion.busy ? "上传中…" : "上传图片"}
            </Button>
          )}
          <Button type="link" onClick={onClose}>
            收起
          </Button>
        </Space>
      }
    >
      <Flex vertical gap={12}>
        <Flex vertical gap={4}>
          {/* 用原生 label 关联：面板不处于 antd Form 内，Form.Item 拿不到表单上下文。 */}
          <label htmlFor="media-insert-alt">替代文字</label>
          <Input
            id="media-insert-alt"
            value={alt}
            onChange={(event) => setAlt(event.target.value)}
            placeholder="描述图片内容；留空则用文件名"
          />
        </Flex>

        {insertion.error !== null && <Alert type="error" showIcon title={insertion.error} />}
        {insertion.notice !== null && <Alert type="success" showIcon title={insertion.notice} />}
        {loadError !== null && <Alert type="error" showIcon title={loadError} />}
        {assets === null && (
          <Flex justify="center" align="center" gap={8} style={{ padding: 12 }}>
            <Spin size="small" />
            <Typography.Text type="secondary">正在加载媒体库…</Typography.Text>
          </Flex>
        )}
        {assets !== null && assets.length === 0 && loadError === null && (
          <Empty
            image={Empty.PRESENTED_IMAGE_SIMPLE}
            description="媒体库还是空的：拖入、粘贴或点击「上传图片」。"
          />
        )}

        {assets !== null && assets.length > 0 && (
          <Flex wrap gap={12}>
            {assets.slice(0, PANEL_PAGE_SIZE).map((asset) => (
              <Card key={asset.id} size="small" style={{ width: 180 }}>
                <Flex vertical gap={8}>
                  <Image
                    src={asset.url}
                    alt={asset.original_name}
                    height={96}
                    style={{ objectFit: "cover" }}
                  />
                  <Typography.Text code title={asset.original_name}>
                    {asset.original_name}
                  </Typography.Text>
                  <Typography.Text type="secondary">
                    {asset.width}×{asset.height} · {formatBytes(asset.byte_size)}
                  </Typography.Text>
                  <Button
                    disabled={insertion.busy}
                    onClick={() =>
                      insertion.insertAsset(
                        asset,
                        alt.trim().length > 0 ? alt : asset.original_name,
                      )
                    }
                  >
                    插入
                  </Button>
                </Flex>
              </Card>
            ))}
          </Flex>
        )}

        <Typography.Text type="secondary">
          提示：也可以直接把图片拖到正文框，或在正文框内粘贴剪贴板图片。
        </Typography.Text>
      </Flex>
    </Card>
  );
}
