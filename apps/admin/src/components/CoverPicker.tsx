import { invalidateAfterWrite } from "../queryEffects";
import {
  Alert,
  Button,
  Card,
  Empty,
  Flex,
  Image,
  Modal,
  Space,
  Spin,
  Typography,
  theme,
} from "antd";
import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { mediaApi } from "../api";
import { permissionMessageOf } from "../apiError";
import { MEDIA_ACCEPT, MEDIA_MAX_BYTES, formatBytes, mediaUrl, uploadRejection } from "../media";
import type { MediaAsset } from "../types";

/** 弹窗一次展示的资产数（与 MediaInsertPanel 相同的「第一页就够用」口径）。 */
const PICKER_PAGE_SIZE = 24;

export interface CoverPickerProps {
  /** 已选封面媒体 id（null = 无封面）。父组件持有，选择器只上报变化。 */
  value: string | null;
  onChange: (id: string | null) => void;
  /** 服务端下发的封面地址；缺省时按 `mediaUrl(value)` 推导。 */
  currentUrl?: string | null;
  /** 没有 media.read 时不提供选择入口（后端同样会 403，这里只是不给死路）。 */
  canReadMedia: boolean;
  /** 没有 media.upload 时不展示上传入口。 */
  canUploadMedia: boolean;
  disabled?: boolean;
  label?: string;
}

/**
 * 封面选择器：预览当前封面 + 打开媒体库弹窗挑选/上传。
 *
 * 有意做成**受控组件**（`value`/`onChange` 与 antd Form.Item 的注入约定一致）：
 * 只上报选中的 id，表单值、脏标记与提交载荷仍由父组件统一持有（文章编辑器里
 * 就是 FormState 的单个可空字段），选择器自己只拥有弹窗与列表状态。
 *
 * 列表只取第一页：需要找更早的图片时去媒体库；上传成功后直接把新资产设为封面，
 * 不再等列表刷新（用户选一张图就是为了用它当封面）。
 */
export function CoverPicker({
  value,
  onChange,
  currentUrl,
  canReadMedia,
  canUploadMedia,
  disabled = false,
  label = "封面",
}: CoverPickerProps) {
  const { token } = theme.useToken();
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [assets, setAssets] = useState<MediaAsset[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const fileInput = useRef<HTMLInputElement | null>(null);
  /**
   * 已知文件名（选择/上传时记下）。服务端只回 id 与地址、不回原文件名，
   * 加载既有封面时这里为 null，界面就不展示文件名。
   */
  const [known, setKnown] = useState<{ id: string; name: string } | null>(null);
  const knownName = known !== null && known.id === value ? known.name : null;

  const load = useCallback(async (): Promise<void> => {
    setError(null);
    try {
      const page = await mediaApi.list(1);
      setAssets(page.items);
    } catch (e) {
      setAssets([]);
      setError(permissionMessageOf(e));
    }
  }, []);

  // 打开才取数、关闭即清空：选择器常驻在表单里，不该在关闭状态下发无谓请求；
  // 清空还能保证关闭后不残留上一次的列表内容。
  useEffect(() => {
    if (open) {
      void load();
    } else {
      setAssets(null);
      setError(null);
    }
  }, [open, load]);

  /** 选中一个资产：记下文件名（如已知）并上报 id，随后收起弹窗。 */
  function choose(asset: MediaAsset): void {
    setKnown({ id: asset.id, name: asset.original_name });
    onChange(asset.id);
    setOpen(false);
  }

  function remove(): void {
    setKnown(null);
    onChange(null);
  }

  async function upload(files: File[]): Promise<void> {
    const file = files[0];
    if (file === undefined) return;
    // 与服务端同一口径的客户端预筛：明显的非图片/空文件/超限先在本地拦下。
    const rejection = uploadRejection(file);
    if (rejection !== null) {
      setError(rejection);
      return;
    }
    setError(null);
    setBusy(true);
    try {
      const uploaded = await mediaApi.upload(file);
      void invalidateAfterWrite(queryClient, "media");
      choose(uploaded);
    } catch (e) {
      setError(permissionMessageOf(e));
    } finally {
      setBusy(false);
    }
  }

  // 没有 media.read：不渲染任何会发出请求的入口，只说明缺哪项权限。
  if (!canReadMedia) {
    return (
      <Flex vertical gap={8} style={{ marginBottom: 16 }}>
        <Typography.Text strong>{label}</Typography.Text>
        <Alert
          type="warning"
          showIcon
          title="没有 media.read 权限，无法从媒体库选择封面。"
          description="请让管理员分配媒体库读取权限后再设置封面。"
        />
      </Flex>
    );
  }

  // 空值收敛：契约是 `string | null`，但历史/测试夹具可能缺字段（undefined），
  // 统一按「未设置」处理，绝不拼出 `/media/undefined` 这种坏地址。
  const hasValue = value !== null && value !== undefined && value !== "";
  const coverUrl = hasValue ? (currentUrl ?? mediaUrl(value)) : null;

  return (
    <Flex vertical gap={8} style={{ marginBottom: 16 }}>
      <Typography.Text strong>{label}</Typography.Text>

      {!hasValue || coverUrl === null ? (
        <Flex align="center" gap={12} wrap>
          {/* 占位区与缩略图同尺寸，避免选择前后布局跳动。 */}
          <Flex
            align="center"
            justify="center"
            style={{
              width: 160,
              height: 90,
              border: `1px dashed ${token.colorBorder}`,
              borderRadius: token.borderRadius,
              background: token.colorFillQuaternary,
            }}
          >
            <Typography.Text type="secondary">未设置封面</Typography.Text>
          </Flex>
          <Button disabled={disabled} onClick={() => setOpen(true)}>
            选择封面
          </Button>
        </Flex>
      ) : (
        <Flex align="center" gap={12} wrap>
          <Image
            src={coverUrl}
            alt={knownName ?? "封面"}
            width={160}
            height={90}
            style={{ objectFit: "cover" }}
          />
          <Flex vertical gap={8}>
            {knownName !== null && (
              <Typography.Text code title={knownName}>
                {knownName}
              </Typography.Text>
            )}
            <Space>
              <Button disabled={disabled} onClick={() => setOpen(true)}>
                更换封面
              </Button>
              <Button danger disabled={disabled} onClick={remove}>
                移除封面
              </Button>
            </Space>
          </Flex>
        </Flex>
      )}

      {/*
        只挂载打开的弹窗：关闭即卸载（antd 默认会把关闭的 Modal 留在 DOM 里），
        避免残留的列表/文件输入，也避免两个弹窗同时存在时的无障碍标签串扰。
      */}
      {open && (
        <Modal
          title="选择图片"
          open
          onCancel={() => setOpen(false)}
          footer={null}
          width={760}
        >
          <Flex vertical gap={12}>
            {error !== null && <Alert type="error" showIcon title={error} />}
            <Flex justify="space-between" align="center" wrap gap={8}>
              <Typography.Text type="secondary">
                从媒体库第一页选择；单张上限 {formatBytes(MEDIA_MAX_BYTES)}，仅
                PNG/JPEG/GIF/WebP。
              </Typography.Text>
              <Space>
                <input
                  ref={fileInput}
                  type="file"
                  accept={MEDIA_ACCEPT}
                  hidden
                  onChange={(event) => {
                    const files = Array.from(event.target.files ?? []);
                    event.target.value = "";
                    void upload(files);
                  }}
                />
                {canUploadMedia && (
                  <Button disabled={busy} onClick={() => fileInput.current?.click()}>
                    {busy ? "上传中…" : "上传图片"}
                  </Button>
                )}
              </Space>
            </Flex>

            {assets === null && (
              <Flex justify="center" align="center" gap={8} style={{ padding: 12 }}>
                <Spin size="small" />
                <Typography.Text type="secondary">正在加载媒体库…</Typography.Text>
              </Flex>
            )}
            {assets !== null && assets.length === 0 && error === null && (
              <Empty
                image={Empty.PRESENTED_IMAGE_SIMPLE}
                description={
                  canUploadMedia ? "媒体库还是空的：点「上传图片」。" : "媒体库还是空的。"
                }
              />
            )}

            {assets !== null && assets.length > 0 && (
              <Flex wrap gap={12}>
                {assets.slice(0, PICKER_PAGE_SIZE).map((asset) => (
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
                        disabled={busy || asset.id === value}
                        onClick={() => choose(asset)}
                      >
                        {asset.id === value ? "当前封面" : "选择"}
                      </Button>
                    </Flex>
                  </Card>
                ))}
              </Flex>
            )}
          </Flex>
        </Modal>
      )}
    </Flex>
  );
}
