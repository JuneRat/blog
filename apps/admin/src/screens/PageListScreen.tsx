import { formatDateTime, useTimeZone } from "../timeZone";
import { statusLabel } from "../components/ContentLifecycleControls";
import { Alert, Button, Flex, Table, Typography } from "antd";
import type { TableProps } from "antd";
import { api } from "../api";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { useContentList } from "../useContentList";
import { ContentListFilters, ContentPagination } from "../components/ContentListControls";
import { navigate, paths } from "../router";
import type { PageSummary } from "../types";

/**
 * 独立页面列表：站点级 page.read，与文章列表（按作者）不同。
 *
 * 导航交给 `AdminLayout` 的左侧菜单，本屏只保留页面说明、自己的空状态与
 * 「新建页面」动作；失败提示沿用内联 `Alert`。
 */
export function PageListScreen() {
  const timeZone = useTimeZone();
  const { me } = useAuth();
  /** 只读列表：失败按管理屏口径加「没有权限：」前缀（本屏没有写操作）。 */
  const { query: pages, filter, setFilter, setPage } = useContentList("pages", api.listPages);
  const errorText = pages.error === null ? null : permissionMessageOf(pages.error);
  const canCreate = me?.permissions.includes("page.create") ?? false;

  const columns: TableProps<PageSummary>["columns"] = [
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
      render: (slug: string) => <Typography.Text code>/{slug}</Typography.Text>,
    },
    {
      title: "标题",
      dataIndex: "title",
      render: (title: string) => title || "（无标题）",
    },
    {
      title: "更新时间",
      dataIndex: "updated_at",
      render: (updatedAt: string) => <Typography.Text type="secondary">{formatDateTime(updatedAt, timeZone)}</Typography.Text>,
    },
  ];

  // 「编辑」是整行点击的键盘可达等价路径（行点击对键盘用户不可用）。
  columns.push({
    title: "操作",
    key: "actions",
    render: (_value, page) => (
      <Button
        type="link"
        onClick={(event) => {
          // 行本身可点进编辑页；操作按钮不能冒泡成一次跳转。
          event.stopPropagation();
          navigate(paths.editPage(page.id));
        }}
      >
        编辑
      </Button>
    ),
  });

  return (
    <>
      <Typography.Title level={3}>独立页面</Typography.Title>

      <Typography.Paragraph type="secondary">
        发布后通过根路径访问（如 /about）。slug 不能占用 admin、api、auth 等系统路径。
      </Typography.Paragraph>

      {errorText !== null && (
        <Alert type="error" showIcon title={errorText} style={{ marginBottom: 16 }} />
      )}

      <Button onClick={() => navigate(paths.pageTrash)} style={{marginBottom:16}}>页面回收站</Button>
      {canCreate && (
        <Flex justify="flex-end" style={{ marginBottom: 16 }}>
          <Button type="primary" onClick={() => navigate(paths.newPage)}>
            新建页面
          </Button>
        </Flex>
      )}

      <ContentListFilters filter={filter} onChange={setFilter} />

      <Table<PageSummary>
        rowKey="id"
        size="middle"
        loading={pages.isFetching}
        dataSource={pages.data?.items ?? []}
        columns={columns}
        pagination={false}
        // 保留迁移前的语义：整行点击进入该页面的编辑页。
        onRow={(page) => ({
          onClick: () => navigate(paths.editPage(page.id)),
          style: { cursor: "pointer" },
        })}
        locale={{
          emptyText:
            errorText !== null
              ? "页面加载失败。"
              : filter.q || filter.status || filter.visibility
                ? "没有符合筛选条件的页面。"
                : `还没有页面。${canCreate ? "点击「新建页面」开始。" : ""}`,
        }}
      />
      <ContentPagination data={pages.data} busy={pages.isFetching} onChange={setPage} />
    </>
  );
}
