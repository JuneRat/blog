import { formatDateTime } from "../timeZone";
import { useTimeZone } from "../timeZoneContext";
import { invalidateAfterWrite } from "../queryEffects";
import { statusLabel } from "../components/ContentLifecycleControls";
import { Alert, App as AntdApp, Button, Flex, Modal, Select, Space, Table, Typography } from "antd";
import type { TableProps } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { postsApi } from "../api/posts";
import { categoryApi } from "../api/taxonomy";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import { useContentList } from "../useContentList";
import { ContentListFilters, ContentPagination } from "../components/ContentListControls";
import { PostScopeFilters } from "../components/PostScopeFilters";
import { navigate, paths } from "../router";
import type { PostSummary } from "../types";

/**
 * 我的文章：按当前作者列出草稿/已发布内容。
 *
 * 导航（独立页面/媒体库/标签/…）由 `AdminLayout` 的左侧菜单负责，本屏只保留
 * 自己的内容与「新建草稿」动作；错误与冲突沿用内联 `Alert`，不用 message 吐司。
 */
export function PostListScreen() {
  const timeZone = useTimeZone();
  const { me } = useAuth();
  const { modal } = AntdApp.useApp();
  /**
   * 列表走 React Query：只负责读取，唯一的写操作（移入回收站）成功后失效重取。
   *
   * 错误分两处：`posts.error` 是**取数失败**（含权限口径文案），`actionError` 是
   * 写操作失败；展示时动作错误优先。
   */
  const { query: posts, filter, setFilter, setPage } = useContentList("posts", postsApi.listPosts);
  const queryClient = useQueryClient();
  const [actionError, setActionError] = useState<string | null>(null);
  const errorText = actionError ?? (posts.error === null ? null : permissionMessageOf(posts.error));
  const canCreate = me?.permissions.includes("post.create") ?? false;
  const canTrash = me?.permissions.some((p) => p === "post.delete" || p === "post.delete_any") ?? false;
  const canPublish = me?.permissions.some((p) => p === "post.publish" || p === "post.publish_any") ?? false;
  const canUnpublish = me?.permissions.some((p) => p === "post.unpublish" || p === "post.unpublish_any") ?? false;
  const canUpdate = me?.permissions.some((p) => p === "post.update" || p === "post.update_any") ?? false;
  // 移入回收站的失败文案沿用同一个 setter（查询只管取数那一次）。
  const setError = setActionError;

  const [selectedRowKeys, setSelectedRowKeys] = useState<React.Key[]>([]);
  const [batchBusy, setBatchBusy] = useState(false);
  const [categoryModalOpen, setCategoryModalOpen] = useState(false);
  const [selectedCategoryId, setSelectedCategoryId] = useState<string | undefined>(undefined);

  const categories = useQuery({
    queryKey: queryKeys.categories(),
    queryFn: categoryApi.list,
    enabled: canUpdate,
  });

  useEffect(() => {
    setSelectedRowKeys([]);
  }, [filter.page, filter.status, filter.visibility, filter.q, filter.scope, filter.author, filter.category_id]);

  const selectedPosts = (posts.data?.items ?? []).filter((p) => selectedRowKeys.includes(p.id));

  async function handleBatchStatus(status: "published" | "draft" | "archived") {
    if (selectedPosts.length === 0) return;
    setBatchBusy(true);
    setError(null);
    try {
      await postsApi.batch({
        action: "change_status",
        params: { status },
        items: selectedPosts.map(p => ({ id: p.id, expected_version: p.version })),
      });
      setSelectedRowKeys([]);
      await invalidateAfterWrite(queryClient, "post");
    } catch (e) {
      setError(permissionMessageOf(e));
      setSelectedRowKeys([]);
      await invalidateAfterWrite(queryClient, "post");
    } finally {
      setBatchBusy(false);
    }
  }

  function handleBatchTrash() {
    if (selectedPosts.length === 0) return;
    modal.confirm({
      title: `将选中的 ${selectedPosts.length} 篇文章移入回收站？`,
      content: "公开入口会立即隐藏。",
      okButtonProps: { danger: true },
      onOk: async () => {
        setBatchBusy(true);
        setError(null);
        try {
          await postsApi.batch({
            action: "trash",
            items: selectedPosts.map(p => ({ id: p.id, expected_version: p.version })),
          });
          setSelectedRowKeys([]);
          await invalidateAfterWrite(queryClient, "post");
        } catch (e) {
          setError(permissionMessageOf(e));
          setSelectedRowKeys([]);
          await invalidateAfterWrite(queryClient, "post");
        } finally {
          setBatchBusy(false);
        }
      },
    });
  }

  async function handleBatchCategory() {
    if (selectedCategoryId === undefined || selectedPosts.length === 0) return;
    setBatchBusy(true);
    setError(null);
    try {
      await postsApi.batch({
        action: "change_category",
        params: { category_id: selectedCategoryId === "" ? null : selectedCategoryId },
        items: selectedPosts.map(p => ({ id: p.id, expected_version: p.version })),
      });
      setCategoryModalOpen(false);
      setSelectedRowKeys([]);
      await invalidateAfterWrite(queryClient, "post");
    } catch (e) {
      setError(permissionMessageOf(e));
      setCategoryModalOpen(false);
      setSelectedRowKeys([]);
      await invalidateAfterWrite(queryClient, "post");
    } finally {
      setBatchBusy(false);
    }
  }

  function trash(post: PostSummary): void {
    modal.confirm({
      title: `将「${post.title || post.slug}」移入回收站？`,
      content: "公开入口会立即隐藏。",
      okButtonProps: { danger: true },
      onOk: async () => {
        setError(null);
        try {
          await postsApi.trashPost(post.id, post.version);
          await invalidateAfterWrite(queryClient, "post");
        } catch (e) {
          setError(permissionMessageOf(e));
        }
      },
    });
  }

  const columns: TableProps<PostSummary>["columns"] = [
    {
      title: "标题",
      dataIndex: "title",
      render: (title: string) => <Typography.Text strong>{title || "（无标题）"}</Typography.Text>,
    },
    { title: "作者", dataIndex: "author_username" },
    {
      title: "状态",
      dataIndex: "status",
      width: 110,
      render: (status: string) => statusLabel(status),
    },
    {
      title: "可见",
      dataIndex: "visibility",
      width: 90,
      render: (visibility: string) => (visibility === "public" ? "公开" : "私有"),
    },
    {
      title: "slug",
      dataIndex: "slug",
      render: (slug: string) => <Typography.Text code>{slug}</Typography.Text>,
    },
    {
      title: "版本",
      dataIndex: "version",
      width: 80,
      render: (version: number) => <Typography.Text type="secondary">v{version}</Typography.Text>,
    },
    {
      title: "更新时间",
      dataIndex: "updated_at",
      width: 180,
      render: (updatedAt: string) => <Typography.Text type="secondary">{formatDateTime(updatedAt, timeZone)}</Typography.Text>,
    },
  ];
  // 操作列始终存在：「编辑」是整行点击的键盘可达等价路径（行点击对键盘用户不可用）。
  columns.push({
    title: "操作",
    key: "actions",
    width: 140,
    render: (_value, post) => (
      <Space>
        <Button
          type="link"
          onClick={(event) => {
            // 行本身可点进编辑页；操作按钮不能冒泡成一次跳转。
            event.stopPropagation();
            navigate(paths.editPost(post.id));
          }}
        >
          编辑
        </Button>
        {canTrash && (post.author_id === me?.user_id || me?.permissions.includes("post.delete_any")) && (
          <Button
            type="text"
            danger
            onClick={(event) => {
              event.stopPropagation();
              trash(post);
            }}
          >
            移入回收站
          </Button>
        )}
      </Space>
    ),
  });

  return (
    <>
      <Typography.Title level={3}>{filter.scope === "all" || filter.author ? "全部文章" : "我的文章"}</Typography.Title>

      {errorText !== null && (
        <Alert type="error" showIcon title={errorText} style={{ marginBottom: 16 }} />
      )}

      <Flex justify="space-between" align="center" style={{ marginBottom: 16 }}>
        <Typography.Paragraph type="secondary" style={{ margin: 0 }}>
          管理已发布的文章与草稿。
        </Typography.Paragraph>
        {canCreate && (
          <Button type="primary" onClick={() => navigate(paths.newPost)}>
            新建草稿
          </Button>
        )}
      </Flex>

      <PostScopeFilters filter={filter} onChange={setFilter} />
      <Flex justify="space-between" align="flex-start" wrap gap={12}>
        <ContentListFilters filter={filter} onChange={setFilter} />
      </Flex>

      {selectedRowKeys.length > 0 && (
        <Flex gap={8} align="center" wrap style={{ background: "#f5f5f5", padding: "8px 12px", borderRadius: 6, marginBottom: 16 }}>
          <Typography.Text strong style={{ fontSize: 13 }}>已选 {selectedRowKeys.length} 篇：</Typography.Text>
          {canTrash && (
            <Button size="small" danger disabled={batchBusy} onClick={handleBatchTrash}>
              批量移入回收站
            </Button>
          )}
          {canPublish && (
            <Button size="small" type="primary" disabled={batchBusy} onClick={() => void handleBatchStatus("published")}>
              批量发布
            </Button>
          )}
          {canUnpublish && (
            <>
              <Button size="small" disabled={batchBusy} onClick={() => void handleBatchStatus("draft")}>
                批量撤回草稿
              </Button>
              <Button size="small" disabled={batchBusy} onClick={() => void handleBatchStatus("archived")}>
                批量归档
              </Button>
            </>
          )}
          {canUpdate && (
            <Button size="small" disabled={batchBusy} onClick={() => { setSelectedCategoryId(undefined); setCategoryModalOpen(true); }}>
              批量修改分类
            </Button>
          )}
          <Button size="small" type="text" onClick={() => setSelectedRowKeys([])}>
            取消选择
          </Button>
        </Flex>
      )}

      <Table<PostSummary>
        rowKey="id"
        rowSelection={{
          selectedRowKeys,
          onChange: setSelectedRowKeys,
        }}
        size="middle"
        loading={posts.isFetching}
        dataSource={posts.data?.items ?? []}
        columns={columns}
        pagination={false}
        // 保留迁移前的语义：整行点击进入该文章的编辑页。
        onRow={(post) => ({
          onClick: (e) => {
            const target = e.target as HTMLElement;
            if (target.closest(".ant-table-selection-column, .ant-checkbox-wrapper, button, a")) {
              return;
            }
            navigate(paths.editPost(post.id));
          },
          style: { cursor: "pointer" },
        })}
        locale={{
          emptyText:
            errorText !== null
              ? "文章加载失败。"
              : filter.q
                ? "没有匹配搜索条件的文章。"
                : filter.status || filter.visibility || filter.category_id || filter.author
                  ? "没有符合筛选条件的文章。"
                  : `还没有文章。${canCreate ? "点击「新建草稿」开始。" : ""}`,
        }}
      />
      <ContentPagination data={posts.data} busy={posts.isFetching} onChange={setPage} />

      <Modal
        title="批量修改分类"
        open={categoryModalOpen}
        onCancel={() => setCategoryModalOpen(false)}
        confirmLoading={batchBusy}
        okButtonProps={{ disabled: selectedCategoryId === undefined }}
        onOk={() => void handleBatchCategory()}
        okText="保存"
        cancelText="取消"
      >
        <Typography.Paragraph type="secondary">
          将选中的 {selectedRowKeys.length} 篇文章分类批量设置为：
        </Typography.Paragraph>
        <Select
          style={{ width: "100%" }}
          placeholder="请选择分类（选择清除分类则解绑分类）"
          allowClear
          value={selectedCategoryId}
          onChange={(val) => setSelectedCategoryId(val ?? undefined)}
          options={[
            { value: "", label: "（清除分类）" },
            ...(categories.data ?? []).map((c) => ({ value: c.id, label: c.name })),
          ]}
        />
      </Modal>
    </>
  );
}
