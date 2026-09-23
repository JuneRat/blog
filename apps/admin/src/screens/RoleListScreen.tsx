import { Alert, Table, Tag, Typography } from "antd";
import type { TableProps } from "antd";
import { useQuery } from "@tanstack/react-query";
import { api } from "../api";
import { messageOf } from "../apiError";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import type { RoleSummary } from "../types";

export function RoleListScreen() {
  const { me } = useAuth();

  const canAdminister =
    (me?.permissions.includes("role.manage") ?? false) ||
    (me?.permissions.includes("user.manage") ?? false);

  /**
   * 角色目录不走 React Query 默认的权限前缀口径：失败文案用 `messageOf`。
   *
   * `enabled` 跟着权限走——无权限时**不发请求**（后端才是权限边界，界面只是不白跑一趟）。
   */
  const roles = useQuery({
    queryKey: queryKeys.roles(),
    queryFn: () => api.listRoles(),
    enabled: canAdminister,
  });
  const errorText = roles.error === null ? null : messageOf(roles.error);

  const columns: TableProps<RoleSummary>["columns"] = [
    {
      title: "slug",
      dataIndex: "slug",
      render: (slug: string) => <Typography.Text code>{slug}</Typography.Text>,
    },
    { title: "名称", dataIndex: "name" },
    {
      title: "类型",
      dataIndex: "builtin",
      render: (builtin: boolean) => <Tag>{builtin ? "内置" : "自定义"}</Tag>,
    },
    { title: "权限数", dataIndex: "permission_count" },
    {
      title: "说明",
      dataIndex: "description",
      render: (description: string | null) => (
        <Typography.Text type="secondary">{description ?? "—"}</Typography.Text>
      ),
    },
  ];

  if (!canAdminister) {
    return (
      <>
        <Typography.Title level={3}>角色目录</Typography.Title>
        {/* 无权限时不调用接口，只说明原因（后端才是权限边界）。 */}
        <Alert
          type="warning"
          showIcon
          title="当前账号没有 role.manage 或 user.manage 权限，无法查看角色。"
        />
      </>
    );
  }

  return (
    <>
      <Typography.Title level={3}>角色目录</Typography.Title>

      <Typography.Paragraph type="secondary">
        内置角色由初始化种子保留，不能通过普通 API 创建、改名或删除；角色分配在「用户与角色」界面完成。
      </Typography.Paragraph>

      {errorText !== null && (
        <Alert type="error" showIcon title={errorText} style={{ marginBottom: 16 }} />
      )}

      <Table<RoleSummary>
        rowKey="slug"
        size="middle"
        loading={roles.isPending}
        dataSource={roles.data ?? []}
        columns={columns}
        pagination={false}
        locale={{
          emptyText: errorText !== null ? "角色目录加载失败。" : "没有角色。",
        }}
      />
    </>
  );
}
