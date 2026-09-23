import { Alert, App as AntdApp, Button, Flex, Table, Typography } from "antd";
import type { TableProps } from "antd";
import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { api } from "../api";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import type { PostSummary } from "../types";

/**
 * 文章回收站：恢复或永久删除已移入回收站的文章。
 *
 * 分页仍由服务端驱动：`requested` 变化即换查询键重新拉取，因此保留原来的上一页/
 * 下一页语义与「第 N 页」文案。永久删除不可撤销，确认弹窗沿用
 * `App.useApp().modal.confirm`（与标签屏一致）。
 *
 * 三个容易踩的边界：
 * - 操作成功后**不清空列表**，只失效重取：重取失败时旧数据仍在（Query 失败会保留
 *   上一次成功的 `data`），用户不会误以为操作失败（操作成功另有提示）；
 * - 当前页被清空时回退一页，不留在一个空页上；
 * - 翻页/重取期间用 `placeholderData` 留住上一页的行与页码，否则分页控件会闪没，
 *   「请求中禁用翻页」就变成了「请求中没有按钮」。
 */
export function PostTrashScreen() {
  const { me } = useAuth();
  const { modal } = AntdApp.useApp();
  const queryClient = useQueryClient();
  /** 请求的是哪一页；**渲染一律用服务端回显的 `data.page`**，两者不混用。 */
  const [requested, setRequested] = useState(1);
  const [actionError, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const canPurge = me?.permissions.includes("post.purge") ?? false;

  const trash = useQuery({
    queryKey: queryKeys.trash(requested),
    queryFn: () => api.listTrash(requested),
    // 保留上一次成功的数据：页码与行都来自服务端回显，翻页期间两者始终自洽。
    placeholderData: keepPreviousData,
  });
  const data = trash.data;
  const errorText =
    actionError ?? (trash.error === null ? null : permissionMessageOf(trash.error));

  /**
   * 删除/恢复后当前页可能已空：回退一页，而不是停在空的「第 N 页」。
   *
   * 只改「请求哪一页」，**不动 data**：页码一律由 `data.page`（服务端回显）渲染，
   * 所以回退期间界面仍显示上一次成功那页的页码与行，两者始终自洽；
   * 否则会出现「第 1 页」旁边还挂着第 2 页数据这种瞬时错配。
   */
  useEffect(() => {
    if (data !== undefined && data.items.length === 0 && requested > 1) {
      setRequested(requested - 1);
    }
  }, [data, requested]);

  async function run(post: PostSummary, purge: boolean): Promise<void> {
    setBusy(post.id);
    setActionError(null);
    setNotice(null);
    try {
      if (purge) await api.purgePost(post.slug, post.version);
      else await api.restorePost(post.slug, post.version);
      // 先给出成功反馈，再重取：即使重取失败，用户也知道操作已经生效。
      setNotice(purge ? `已永久删除「${post.title || post.slug}」。` : `已恢复「${post.title || post.slug}」。`);
      /**
       * 失效**整个回收站缓存**，而不只是当前页：页码是查询键的一部分，
       * 只失效当前页会留下其他页的陈旧数据（总数与被删行都变了）。
       * 用 `queryKeys.trashAll()`（`["trash"]` 前缀），而不是当前页的键。
       */
      await queryClient.invalidateQueries({ queryKey: queryKeys.trashAll() });
      /**
       * 恢复会让文章重新出现在「我的文章」列表里，所以必须同时失效文章列表；
       * 永久删除不会改变列表（它本来就不在列表里），故只在恢复时做。
       */
      if (!purge) await queryClient.invalidateQueries({ queryKey: queryKeys.posts() });
    } catch (e) {
      setActionError(permissionMessageOf(e));
    } finally {
      setBusy(null);
    }
  }

  function act(post: PostSummary, purge: boolean): void {
    if (!purge) {
      void run(post, false);
      return;
    }
    modal.confirm({
      title: `永久删除「${post.title || post.slug}」？`,
      content: "文章、标签关联与系列位置将无法恢复。",
      okButtonProps: { danger: true },
      onOk: () => run(post, true),
    });
  }

  const columns: TableProps<PostSummary>["columns"] = [
    {
      title: "标题",
      dataIndex: "title",
      render: (title: string) => title || "（无标题）",
    },
    {
      title: "slug",
      dataIndex: "slug",
      render: (slug: string) => <Typography.Text code>{slug}</Typography.Text>,
    },
    { title: "原状态", dataIndex: "status" },
    {
      title: "版本",
      dataIndex: "version",
      render: (version: number) => <Typography.Text type="secondary">v{version}</Typography.Text>,
    },
    {
      title: "操作",
      key: "actions",
      render: (_value, post) => (
        <Flex gap={8}>
          <Button type="text" disabled={busy !== null} onClick={() => act(post, false)}>
            恢复
          </Button>
          {canPurge && (
            <Button type="text" danger disabled={busy !== null} onClick={() => act(post, true)}>
              永久删除
            </Button>
          )}
        </Flex>
      ),
    },
  ];

  return (
    <>
      <Typography.Title level={3}>文章回收站</Typography.Title>

      <Typography.Paragraph type="secondary">
        恢复后的文章为草稿；原归档文章仍为归档，不会自动发布。永久删除不可撤销。
      </Typography.Paragraph>

      {errorText !== null && (
        <Alert type="error" showIcon title={errorText} style={{ marginBottom: 16 }} />
      )}
      {notice !== null && (
        <Alert type="success" showIcon title={notice} style={{ marginBottom: 16 }} />
      )}

      {data !== undefined && (
        <Typography.Paragraph type="secondary">共 {data.total} 篇</Typography.Paragraph>
      )}

      <Table<PostSummary>
        rowKey="id"
        size="middle"
        loading={trash.isFetching}
        dataSource={data?.items ?? []}
        columns={columns}
        pagination={false}
        locale={errorText !== null ? { emptyText: "回收站加载失败。" } : undefined}
      />

      {data !== undefined && (
        <Flex gap={12} align="center" style={{ marginTop: 16 }}>
          <Button disabled={trash.isFetching || data.page <= 1} onClick={() => setRequested(data.page - 1)}>
            上一页
          </Button>
          <Typography.Text>第 {data.page} 页</Typography.Text>
          <Button
            disabled={trash.isFetching || data.page * data.per_page >= data.total}
            onClick={() => setRequested(data.page + 1)}
          >
            下一页
          </Button>
        </Flex>
      )}
    </>
  );
}
