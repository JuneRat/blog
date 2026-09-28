import { formatDateTime, useTimeZone } from "../timeZone";
import { invalidateAfterWrite } from "../queryEffects";
import {
  Alert,
  App as AntdApp,
  Button,
  Card,
  Col,
  Empty,
  Flex,
  Pagination,
  Row,
  Segmented,
  Space,
  Typography,
  Upload,
} from "antd";
import { mediaPageQuery } from "../mediaQueries";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { mediaApi } from "../api";
import { useAuth } from "../auth";
import { navigate, paths } from "../router";
import type { MediaAsset, MediaReference, MediaUsageView } from "../types";
import { MEDIA_ACCEPT, formatBytes } from "../media";
import { uploadImages } from "../components/useImageInsertion";
import { permissionMessageOf } from "../apiError";
import { queryKeys } from "../queryClient";

const { Dragger } = Upload;

/**
 * 引用来源 → 展示名与站内跳转目标。
 *
 * 系列封面是**整个系列**引用图片（系列本身没有单独编辑页），因此统一回到系列目录，
 * 不能沿用页面编辑地址——那会把用户带到 `pages/{id}/edit` 这个不存在的页面。
 * 头像与站点 logo 同理：回到用户列表与站点设置。
 */
function referenceTarget(
  kind: MediaReference["kind"],
  contentId: string,
): { label: string; to: string } {
  switch (kind) {
    case "post":
      return { label: "文章", to: paths.editPost(contentId) };
    case "page":
      return { label: "页面", to: paths.editPage(contentId) };
    case "series":
      return { label: "系列", to: paths.series };
    case "user":
      return { label: "用户", to: paths.users };
    case "site":
      return { label: "站点设置", to: paths.settings };
  }
}

/**
 * 引用状态文案。
 *
 * 文章/页面有发布状态；系列目录、头像与站点 logo 没有「草稿」这一态，
 * 只说明是否公开可读——照搬「已发布/草稿」会把它们误标成草稿。
 */
function referenceStatus(reference: MediaReference): string {
  const publicText = reference.public ? "公开可读" : "不公开";
  if (reference.kind === "post" || reference.kind === "page") {
    const state = reference.deleted
      ? "回收站"
      : reference.status === "published"
        ? "已发布"
        : "草稿";
    return `${state}；${publicText}`;
  }
  return publicText;
}

/** 媒体库与回收站共享稳定公开链接；使用位置仍按来源内容权限过滤。 */
export function MediaLibraryScreen() {
  const timeZone = useTimeZone();
  const { me } = useAuth();
  const { modal } = AntdApp.useApp();
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const [trash, setTrash] = useState(false);
  /**
   * 列表查询：翻页即换键（每页一份缓存）。
   *
   * 错误分两处：`media.error` 是**取数失败**（含权限口径文案），
   * `actionError` 是上传/删除等写操作失败；展示时动作错误优先。
   */
  const media = useQuery(mediaPageQuery(page, trash));
  const [actionError, setActionError] = useState<string | null>(null);
  const errorText =
    actionError ?? (media.error === null ? null : permissionMessageOf(media.error));
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /**
   * 要展示使用位置的资产 id（`null` = 不展示）。
   *
   * 使用位置是另一份服务端状态，用**独立查询**按 id 缓存；`enabled` 让它在
   * 需要时才请求，不必把详情混进列表数据里。
   */
  const [usageId, setUsageId] = useState<string | null>(null);
  const usage = useQuery({
    queryKey: queryKeys.mediaUsage(usageId ?? ""),
    queryFn: (): Promise<MediaUsageView> => {
      // enabled 保证了这里 usageId 非空；显式收窄，避免把空 id 当成合法查询。
      if (usageId === null) return Promise.reject(new Error("no media id for usage query"));
      return mediaApi.detail(usageId);
    },
    enabled: usageId !== null,
  });
  /**
   * 已交给 `upload` 的文件批次。
   *
   * antd 的 `beforeUpload` 会对同一批选中的**每个文件**各调用一次，但每次传入的
   * `fileList` 都是同一个批次数组；用数组引用去重，保证一批只上传一次，顺序与
   * 迁移前「一次 change 事件取全部 files」一致。
   */
  const handledBatch = useRef<readonly File[] | null>(null);

  const canUpload = me?.permissions.includes("media.upload") ?? false;
  const canDeleteAny = me?.permissions.includes("media.delete_any") ?? false;
  const canDeleteOwn = me?.permissions.includes("media.delete") ?? false;
  /** 当前用户的 user_id：`media.delete` 只看自己的资产，用它比对 `asset.owner_id`。 */
  const viewerId = me?.user_id ?? null;

  async function upload(files: File[]): Promise<void> {
    if (files.length === 0) return;
    setActionError(null);
    setNotice(null);
    setBusy(true);
    try {
      const uploaded = await uploadImages(files, () => { void invalidateAfterWrite(queryClient, "media"); });
      setNotice(`已上传 ${uploaded.length} 张图片。`);
      setTrash(false);
      setPage(1);
      // 上传使全部页内容移位（新资产排在最前）：整族失效，与回收站 trashAll 同理；
      // 非活跃页的失效标记会在切回该页时触发重取，不会留下陈旧列表。
      await invalidateAfterWrite(queryClient, "media");
    } catch (e) {
      setActionError(permissionMessageOf(e));
    } finally {
      setBusy(false);
    }
  }

  /**
   * 上传交由本屏自己处理（串行、客户端预筛、错误提示都与迁移前一致）。
   *
   * 返回 `Upload.LIST_IGNORE`：不要让 antd 再去请求，也不要在它内部维护一份
   * 文件列表——上传进度与结果由 `busy`/`error`/`notice` 表达，列表另有网格。
   */
  function takeBatch(batch: File[]): string {
    if (handledBatch.current !== batch) {
      handledBatch.current = batch;
      void upload([...batch]);
    }
    return Upload.LIST_IGNORE;
  }

  function canDelete(asset: MediaAsset): boolean {
    if (canDeleteAny) return true;
    return canDeleteOwn && viewerId !== null && asset.owner_id === viewerId;
  }

  async function changeDeleted(asset: MediaAsset, deleted: boolean): Promise<void> {
    setActionError(null);
    setNotice(null);
    setBusy(true);
    try {
      if (deleted) await mediaApi.remove(asset.id, asset.version);
      else await mediaApi.restore(asset.id, asset.version);
      setNotice(`${asset.original_name} 已${deleted ? "移入回收站" : "恢复"}。`);
      setUsageId(null);
      // 内容会跨页移动，同时失效正常库、回收站和使用位置。
      await invalidateAfterWrite(queryClient, "media");
      if ((media.data?.items.length ?? 0) === 1 && page > 1) setPage(page - 1);
    } catch (e) {
      setActionError(permissionMessageOf(e));
    } finally {
      setBusy(false);
    }
  }

  function remove(asset: MediaAsset): void {
    modal.confirm({
      title: `将「${asset.original_name}」移入回收站？`,
      content: "图片链接仍公开可访问，已有引用会保留。恢复后可以重新选择。",
      okButtonProps: { danger: true },
      onOk: () => changeDeleted(asset, true),
    });
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

  const data = media.data ?? null;
  const pageSize = data?.per_page || 1;
  const totalPages = data === null ? 0 : Math.max(1, Math.ceil(data.total / pageSize));

  return (
    <>
      <Flex justify="space-between" align="center" wrap gap={8} style={{ marginBottom: 16 }}>
        <Typography.Title level={3} style={{ margin: 0 }}>
          媒体库
        </Typography.Title>
        <Space>
          {data !== null && <Typography.Text type="secondary">共 {data.total} 张</Typography.Text>}
          {canUpload && (
            <Upload
              accept={MEDIA_ACCEPT}
              multiple
              disabled={busy}
              showUploadList={false}
              beforeUpload={(_file, batch) => takeBatch(batch)}
            >
              <Button disabled={busy}>{busy ? "处理中…" : "上传图片"}</Button>
            </Upload>
          )}
        </Space>
      </Flex>

      <Segmented
        aria-label="媒体范围"
        options={[{ label: "全部图片", value: "active" }, { label: "回收站", value: "trash" }]}
        value={trash ? "trash" : "active"}
        onChange={(value) => { setTrash(value === "trash"); setPage(1); setUsageId(null); setActionError(null); }}
        style={{ marginBottom: 16 }}
      />
      <Alert
        type="info"
        showIcon
        title="图片链接独立公开。文章设为私密或图片移入回收站，都不会限制链接访问。"
        style={{ marginBottom: 16 }}
      />
      {usage.error !== null && <Alert type="error" title={permissionMessageOf(usage.error)} />}

      {errorText !== null && (
        <Alert type="error" showIcon title={errorText} style={{ marginBottom: 16 }} />
      )}
      {notice !== null && (
        <Alert type="success" showIcon title={notice} style={{ marginBottom: 16 }} />
      )}

      {usage.data !== undefined && (
        <Alert
          type="info"
          showIcon
          style={{ marginBottom: 16 }}
          title={`「${usage.data.media.original_name}」的使用位置`}
          description={
            <>
              <ul style={{ margin: "0 0 8px", paddingInlineStart: 20 }}>
                {usage.data.references.map((reference) => {
                  const target = referenceTarget(reference.kind, reference.content_id);
                  return (
                    <li key={`${reference.kind}-${reference.content_id}`}>
                      <Button
                        type="link"
                        style={{ padding: 0, height: "auto" }}
                        onClick={() => navigate(target.to)}
                      >
                        {target.label}：
                        {reference.title || reference.slug}
                      </Button>
                      <Typography.Text type="secondary">
                        {" "}
                        {referenceStatus(reference)}
                      </Typography.Text>
                    </li>
                  );
                })}
              </ul>
              {usage.data.hidden_references > 0 && (
                <Typography.Paragraph type="secondary" style={{ marginBottom: 0 }}>
                  另有 {usage.data.hidden_references} 处引用你无权查看（他人草稿、私密内容，
                  或你没有页面读取权限的内容）。
                </Typography.Paragraph>
              )}
              <Typography.Paragraph type="secondary" style={{ marginBottom: 0 }}>
                这里只统计站内引用，站外使用不在统计范围内。
              </Typography.Paragraph>
            </>
          }
        />
      )}

      {canUpload && (
        <Dragger
          accept={MEDIA_ACCEPT}
          multiple
          disabled={busy}
          showUploadList={false}
          beforeUpload={(_file, batch) => takeBatch(batch)}
          style={{ marginBottom: 16 }}
        >
          <Typography.Text>
            把图片拖到这里上传；也可以在文章或页面编辑器里直接拖入、粘贴。
          </Typography.Text>
        </Dragger>
      )}

      {data === null && media.isPending && (
        <Typography.Text type="secondary">正在加载…</Typography.Text>
      )}
      {data !== null && data.items.length === 0 && errorText === null && (
        <Empty description={trash ? "回收站是空的。" : "媒体库还是空的。"} />
      )}

      {data !== null && data.items.length > 0 && (
        <>
          <Row gutter={[16, 16]}>
            {data.items.map((asset) => (
              <Col key={asset.id} xs={24} sm={12} lg={8}>
                <Card
                  size="small"
                  styles={{ body: { padding: 12 } }}
                  cover={
                    <img
                      src={asset.url}
                      alt={asset.original_name}
                      loading="lazy"
                      style={{ height: 160, objectFit: "cover" }}
                    />
                  }
                  actions={[
                    <Button key="copy" type="text" onClick={() => void copy(asset)}>
                      复制地址
                    </Button>,
                    <Typography.Link
                      key="preview"
                      href={asset.url}
                      target="_blank"
                      rel="noreferrer"
                    >
                      预览
                    </Typography.Link>,
                    <Button
                      key="delete"
                      type="text"
                      danger={!trash}
                      disabled={busy || !canDelete(asset)}
                      title={canDelete(asset) ? undefined : "需要 media.delete（本人上传）或 media.delete_any"}
                      onClick={() => trash ? void changeDeleted(asset, false) : remove(asset)}
                    >
                      {trash ? "恢复" : "移入回收站"}
                    </Button>,
                  ]}
                >
                  <Flex vertical gap={4}>
                    <Typography.Text code title={asset.original_name}>
                      {asset.original_name}
                    </Typography.Text>
                    <Typography.Text type="secondary">
                      {asset.width}×{asset.height} · {formatBytes(asset.byte_size)} ·{" "}
                      {asset.owner_display}
                    </Typography.Text>
                    <Button type="link" style={{ padding: 0, width: "fit-content", height: "auto" }} onClick={() => setUsageId(asset.id)}>
                      {asset.reference_count === 0 ? "查看使用位置" : `被 ${asset.reference_count} 处引用`}
                    </Button>
                    <Typography.Text type="secondary">{formatDateTime(asset.created_at, timeZone)}</Typography.Text>
                  </Flex>
                </Card>
              </Col>
            ))}
          </Row>
          {totalPages > 1 && (
            <Flex justify="center" style={{ marginTop: 24 }}>
              <Pagination
                current={page}
                pageSize={pageSize}
                total={data.total}
                showSizeChanger={false}
                onChange={(next) => setPage(next)}
              />
            </Flex>
          )}
        </>
      )}
    </>
  );
}
