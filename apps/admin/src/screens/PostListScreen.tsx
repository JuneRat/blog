import { invalidateAfterWrite } from "../queryEffects";
import { statusLabel } from "../components/ContentLifecycleControls";
import { Alert, App as AntdApp, Button, Flex, Space, Table, Typography } from "antd";
import type { TableProps } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { api } from "../api";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import { navigate, paths } from "../router";
import type { PostSummary } from "../types";

/**
 * 我的文章：按当前作者列出草稿/已发布内容。
 *
 * 导航（独立页面/媒体库/标签/…）由 `AdminLayout` 的左侧菜单负责，本屏只保留
 * 自己的内容与「新建草稿」动作；错误与冲突沿用内联 `Alert`，不用 message 吐司。
 */
export function PostListScreen() {
  const { me } = useAuth();
  const { modal } = AntdApp.useApp();
  /**
   * 列表走 React Query：只负责读取，唯一的写操作（移入回收站）成功后失效重取。
   *
   * 错误分两处：`posts.error` 是**取数失败**（含权限口径文案），`actionError` 是
   * 写操作失败；展示时动作错误优先。
   */
  const posts = useQuery({ queryKey: queryKeys.posts(), queryFn: () => api.listPosts() });
  const queryClient = useQueryClient();
  const [actionError, setActionError] = useState<string | null>(null);
  const errorText = actionError ?? (posts.error === null ? null : permissionMessageOf(posts.error));
  const canCreate = me?.permissions.includes("post.create") ?? false;
  const canTrash = me?.permissions.some((p) => p === "post.delete" || p === "post.delete_any") ?? false;
  // 移入回收站的失败文案沿用同一个 setter（查询只管取数那一次）。
  const setError = setActionError;

  function trash(post: PostSummary): void {
    modal.confirm({
      title: `将「${post.title || post.slug}」移入回收站？`,
      content: "公开入口会立即隐藏。",
      okButtonProps: { danger: true },
      onOk: async () => {
        setError(null);
        try {
          await api.trashPost(post.id, post.version);
          await invalidateAfterWrite(queryClient, "post");
        } catch (e) {
          setError(permissionMessageOf(e));
        }
      },
    });
  }

  const columns: TableProps<PostSummary>["columns"] = [
    {
      title: "版本",
      dataIndex: "version",
      render: (version: number) => <Typography.Text type="secondary">v{version}</Typography.Text>,
    },
    {
      title: "状态",
      dataIndex: "status",
      render: (status: string) => statusLabel(status),
    },
    {
      title: "可见",
      dataIndex: "visibility",
      render: (visibility: string) => (visibility === "public" ? "公开" : "私有"),
    },
    {
      title: "slug",
      dataIndex: "slug",
      render: (slug: string) => <Typography.Text code>{slug}</Typography.Text>,
    },
    {
      title: "标题",
      dataIndex: "title",
      render: (title: string) => title || "（无标题）",
    },
    {
      title: "更新时间",
      dataIndex: "updated_at",
      render: (updatedAt: string) => <Typography.Text type="secondary">{updatedAt}</Typography.Text>,
    },
  ];
  // 操作列始终存在：「编辑」是整行点击的键盘可达等价路径（行点击对键盘用户不可用）。
  columns.push({
    title: "操作",
    key: "actions",
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
        {canTrash && (
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
      <Typography.Title level={3}>我的文章</Typography.Title>

      {errorText !== null && (
        <Alert type="error" showIcon title={errorText} style={{ marginBottom: 16 }} />
      )}

      <Flex justify="space-between" align="center" style={{ marginBottom: 16 }}>
        <Typography.Text type="secondary">{me?.permissions.length ?? 0} 项权限</Typography.Text>
        {canCreate && (
          <Button type="primary" onClick={() => navigate(paths.newPost)}>
            新建草稿
          </Button>
        )}
      </Flex>

      <Table<PostSummary>
        rowKey="id"
        size="middle"
        loading={posts.isPending}
        dataSource={posts.data ?? []}
        columns={columns}
        pagination={false}
        // 保留迁移前的语义：整行点击进入该文章的编辑页。
        onRow={(post) => ({
          onClick: () => navigate(paths.editPost(post.id)),
          style: { cursor: "pointer" },
        })}
        locale={{
          emptyText:
            errorText !== null
              ? "文章加载失败。"
              : `还没有文章。${canCreate ? "点击「新建草稿」开始。" : ""}`,
        }}
      />
    </>
  );
}
