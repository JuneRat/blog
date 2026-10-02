import {
  Alert,
  Button,
  Flex,
  Form,
  Input,
  Pagination,
  Popconfirm,
  Select,
  Space,
  Table,
  Tag,
  Typography,
} from "antd";
import type { TableProps } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { ApiError, withRequestId } from "../api/client";
import { identityApi } from "../api/identity";
import { messageOf as apiMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import type { AdminUser, RoleSummary } from "../types";

const USER_PAGE_SIZE = 50;

/**
 * 账号管理界面：列出账号、创建账号、分配/移除角色。
 *
 * 权限由后端执行，这里只据 `/me` 的权限决定「展示哪些控件」，避免把 403
 * 当作正常交互；路由守卫同理只改善体验。所有写操作都走受保护的 API
 * （会话 + CSRF），并按业务码给出精确文案：
 * - `username_taken` / `email_taken`：创建失败的具体原因；
 * - `last_admin`：解释最后一个可登录 Admin 为何不能被移除；
 * - `forbidden`：可能是缺少 admin.manage 或超出委派上限。
 *
 * 这些文案统一用**内联 Alert 而不是 message 吐司**：冲突、权限与最后 Admin
 * 的说明需要停留在屏幕上，3 秒后自动消失会让人来不及看清。
 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    switch (error.code) {
      case "username_taken":
        return "用户名已被占用，请换一个。";
      case "email_taken":
        return "邮箱已被其他账号使用，请换一个或留空。";
      case "last_admin":
        return "这是最后一个可登录的 Admin，不能停用或移除其 Admin 角色；请先确保另一个 Admin 可以登录。";
      case "version_conflict":
        return "账号已在其他位置更新，请核对最新列表后重试。";
      case "forbidden":
        return withRequestId(
          "没有权限执行该操作：可能缺少 admin.manage，或超出了你的委派上限（不能授予自己不具备的权限）。",
          error.requestId,
        );
      default:
        // 其余情况回落到通用文案（含 requestId），不再各写一份 tail。
        return apiMessageOf(error);
    }
  }
  return apiMessageOf(error);
}

/** 创建账号表单的字段；用 antd Form 托管，提交时读回同一份值。 */
interface CreateUserDraft {
  username: string;
  email: string;
  displayName: string;
}

const EMPTY_CREATE: CreateUserDraft = { username: "", email: "", displayName: "" };

/** 登录方式独立于启用状态展示；停用并不删除密码或外部身份。 */
function loginLabel(user: AdminUser): string {
  if (!user.password_enabled && user.external_identities === 0) return "无（无法登录）";
  if (user.password_enabled && user.external_identities > 0) return "密码 + 外部身份";
  if (user.password_enabled) return "本地密码";
  return "外部身份";
}

export function UserListScreen() {
  const { me, refresh } = useAuth();
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const [actionError, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [form] = Form.useForm<CreateUserDraft>();
  /**
   * 表单当前值的一份镜像，只用来决定「创建账号」按钮是否可用：用户名为空时不提交
   * （与迁移前一致）。
   *
   * 用 `onValuesChange` 的同步回调而不是 `Form.useWatch`：后者的通知由
   * rc-field-form 的 WatcherCenter 用 MessageChannel 批处理成宏任务，字段值到
   * 按钮状态之间会差一拍，紧接着 change 的点击会落在仍禁用的按钮上。
   */
  const [createDraft, setCreateDraft] = useState<CreateUserDraft>(EMPTY_CREATE);

  const canManageUsers = me?.permissions.includes("user.manage") ?? false;
  const canManageRoles = me?.permissions.includes("role.manage") ?? false;
  const canAdminship = me?.permissions.includes("admin.manage") ?? false;
  const canAdminister = canManageUsers || canManageRoles;

  /**
   * 账号列表与角色目录各走一个查询。
   *
   * - `enabled` 让**权限不足时不发请求**（无 user.manage/role.manage 的会话
   *   不该用一次必然 403 的调用去试探后端）；
   * - 角色目录失败不能退化成空列表：`roles.error` 单独可见、可重试，
   *   否则一次网络故障会被误读成「没有可分配的角色」，静默阻断分配。
   */
  const users = useQuery({
    queryKey: queryKeys.userList(page),
    queryFn: () => identityApi.listUsers(page, USER_PAGE_SIZE),
    enabled: canAdminister,
  });
  useEffect(() => {
    if (!users.data || users.isFetching || users.isError) return;
    const lastPage = Math.max(1, Math.ceil(users.data.total / users.data.per_page));
    if (page > lastPage) setPage(lastPage);
  }, [page, users.data, users.isFetching, users.isError]);
  const roles = useQuery({
    queryKey: queryKeys.roles(),
    queryFn: () => identityApi.listRoles(),
    enabled: canManageRoles,
  });
  /** 取数失败与写操作失败分开：展示时动作错误优先。 */
  const errorText = actionError ?? (users.error === null ? null : messageOf(users.error));
  const rolesError = roles.error === null ? null : messageOf(roles.error);
  // 屏内多处直接设置文案，沿用同一个 setter。
  const setError = setActionError;

  /** 写操作后让账号列表失效重取（替代手写的「再拉一次」）。 */
  const load = () => queryClient.invalidateQueries({ queryKey: queryKeys.users() });

  async function createUser(draft: CreateUserDraft): Promise<void> {
    const email = (draft.email ?? "").trim();
    const displayName = (draft.displayName ?? "").trim();
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      const created = await identityApi.createUser({
        username: (draft.username ?? "").trim(),
        email: email === "" ? undefined : email,
        display_name: displayName === "" ? undefined : displayName,
      });
      form.resetFields();
      setCreateDraft(EMPTY_CREATE);
      setNotice(`已创建账号 ${created.username}；请为其分配角色并绑定登录方式。`);
      await load();
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function mutateRole(
    user: AdminUser,
    role: string,
    action: "assign" | "remove",
  ): Promise<void> {
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      if (action === "assign") {
        await identityApi.assignRole(user.username, role);
      } else {
        await identityApi.removeRole(user.username, role);
      }
      setNotice(
        action === "assign"
          ? `已为 ${user.username} 分配角色 ${role}。`
          : `已移除 ${user.username} 的角色 ${role}。`,
      );
      // 角色变更保持登录；目标是自己时重新读取权限与资料版本。
      if (user.id === me?.user_id) {
        await refresh();
      }
      await load();
    } catch (e) {
      setError(messageOf(e));
      if (e instanceof ApiError && e.code === "last_admin") await load();
    } finally {
      setBusy(false);
    }
  }

  async function changeStatus(user: AdminUser): Promise<void> {
    if (busy) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    const status = user.status === "active" ? "disabled" : "active";
    try {
      await identityApi.changeUserStatus(user.id, status, user.version);
      setNotice(status === "disabled"
        ? `已停用 ${user.username}，其登录会话已撤销。`
        : `已启用 ${user.username}，该账号需要重新登录。`);
      if (user.id === me?.user_id) {
        await refresh();
      } else {
        await load();
      }
    } catch (e) {
      setError(messageOf(e));
      if (e instanceof ApiError && (e.code === "version_conflict" || e.code === "last_admin")) {
        await load();
      }
    } finally {
      setBusy(false);
    }
  }

  const columns: TableProps<AdminUser>["columns"] = [
    {
      title: "用户名",
      dataIndex: "username",
      render: (_value, user) => (
        <Space size={4}>
          <Typography.Text code>{user.username}</Typography.Text>
          {user.id === me?.user_id && <Tag color="blue">我</Tag>}
        </Space>
      ),
    },
    {
      title: "展示名",
      dataIndex: "display_name",
      render: (displayName: string | null) => displayName ?? "（未设置）",
    },
    {
      title: "邮箱",
      dataIndex: "email",
      render: (email: string | null) => (
        <Typography.Text type="secondary">{email ?? "—"}</Typography.Text>
      ),
    },
    {
      title: "角色",
      dataIndex: "roles",
      render: (_value, user) => (
        <Space size={[4, 4]} wrap>
          {user.roles.length === 0 && (
            <Typography.Text type="secondary">（无）</Typography.Text>
          )}
          {user.roles.map((role) => {
            const lastAdmin = role === "admin" && user.is_last_loginable_admin;
            return (
              <Tag key={role}>
                {role}
                {lastAdmin && (
                  <Typography.Text
                    type="secondary"
                    title="最后一个可登录的 Admin"
                  >
                    （最后 Admin）
                  </Typography.Text>
                )}
                {canManageRoles && (
                  <Button
                    type="link"
                    size="small"
                    disabled={busy || lastAdmin || user.deleted || user.status === "disabled"}
                    title={
                      lastAdmin
                        ? "这是最后一个可登录的 Admin，不能移除其 Admin 角色"
                        : undefined
                    }
                    aria-label={`移除 ${user.username} 的角色 ${role}`}
                    onClick={() => void mutateRole(user, role, "remove")}
                  >
                    移除
                  </Button>
                )}
              </Tag>
            );
          })}
        </Space>
      ),
    },
    {
      title: "状态",
      key: "status",
      render: (_value, user) => user.deleted ? <Tag>已删除</Tag> : (
        <Tag color={user.status === "active" ? "green" : "default"}>
          {user.status === "active" ? "已启用" : "已停用"}
        </Tag>
      ),
    },
    {
      title: "登录方式",
      key: "login",
      render: (_value, user) => (
        <Typography.Text type="secondary">{loginLabel(user)}</Typography.Text>
      ),
    },
  ];
  if (canManageUsers) {
    columns.push({
      title: "账号操作",
      key: "statusAction",
      render: (_value, user) => {
        const disabling = user.status === "active";
        const protectedAdmin = user.roles.includes("admin") && !canAdminship;
        const disabled = busy || user.deleted || protectedAdmin || (disabling && user.is_last_loginable_admin);
        return (
          <Popconfirm
            title={disabling ? `停用 ${user.username}？` : `启用 ${user.username}？`}
            description={disabling
              ? user.id === me?.user_id ? "你将退出登录，需要其他管理员重新启用此账号。" : "该账号将无法登录，现有登录会话会被撤销。"
              : "该账号保留原有角色和登录方式，需要重新登录。"}
            okText={disabling ? "确认停用" : "确认启用"}
            cancelText="取消"
            okButtonProps={{ danger: disabling }}
            disabled={disabled}
            onConfirm={() => changeStatus(user)}
          >
            <Button danger={disabling} disabled={disabled}
              aria-label={`${disabling ? "停用" : "启用"} ${user.username}`}
              title={user.is_last_loginable_admin ? "不能停用最后一个可登录的 Admin"
                : protectedAdmin ? "操作 Admin 需要管理员管理权限" : undefined}>
              {disabling ? "停用" : "启用"}
            </Button>
          </Popconfirm>
        );
      },
    });
  }
  if (canManageRoles) {
    columns.push({
      title: "分配角色",
      key: "assign",
      render: (_value, user) => {
        if (user.deleted || user.status === "disabled") {
          return <Typography.Text type="secondary">账号未启用</Typography.Text>;
        }
        // 角色目录读取失败不能退化成「空目录」：否则一次网络故障会被误读为
        // 「没有可分配的角色」，静默阻断分配。两种状态给出不同文案。
        if (rolesError !== null) {
          return <Typography.Text type="secondary">角色目录不可用</Typography.Text>;
        }
        if (roles.isPending) {
          return <Typography.Text type="secondary">正在加载角色…</Typography.Text>;
        }
        const assignable = (roles.data ?? []).filter(
          (role) =>
            !user.roles.includes(role.slug) &&
            // 授予 Admin 需要专门的管理员管理权限；没有就不展示该选项。
            (role.slug !== "admin" || canAdminship),
        );
        if (assignable.length === 0) {
          return <Typography.Text type="secondary">暂无可分配角色</Typography.Text>;
        }
        return (
          <RoleAssigner
            username={user.username}
            roles={assignable}
            busy={busy}
            onAssign={(role) => void mutateRole(user, role, "assign")}
          />
        );
      },
    });
  }

  if (!canAdminister) {
    return (
      <>
        <Typography.Title level={3}>用户与角色</Typography.Title>
        {/* 无权限时不调用任何账号接口，只说明原因（后端才是权限边界）。 */}
        <Alert
          type="warning"
          showIcon
          title="当前账号没有 user.manage 或 role.manage 权限，无法查看或管理账号。"
        />
      </>
    );
  }

  // 最后一个「可登录」Admin 的判定来自后端的**全局**结果（`is_last_loginable_admin`），
  // 不看当前页：另一个可登录 Admin 落在后续页时，按页推断会把它误判成最后 Admin。
  // 界面据此提前禁用移除，后端执行时仍会在排他锁下复核（前端不是安全边界）。
  return (
    <>
      <Typography.Title level={3}>用户与角色</Typography.Title>

      {errorText !== null && (
        <Alert type="error" showIcon title={errorText} style={{ marginBottom: 16 }}
          action={users.isError && <Button size="small" disabled={users.isFetching} onClick={() => void users.refetch()}>重试账号列表</Button>} />
      )}
      {notice !== null && (
        <Alert type="success" showIcon title={notice} style={{ marginBottom: 16 }} />
      )}
      {canManageRoles && rolesError !== null && (
        <Alert
          type="error"
          showIcon
          style={{ marginBottom: 16 }}
          title={`角色目录加载失败：${rolesError}`}
          action={
            <Button size="small" onClick={() => void roles.refetch()}>
              重试
            </Button>
          }
        />
      )}

      {canManageUsers && (
        <>
          <Typography.Title level={5} style={{ marginTop: 0 }}>
            创建账号
          </Typography.Title>
          <Form
            form={form}
            layout="inline"
            initialValues={EMPTY_CREATE}
            onValuesChange={(_changed, values) => setCreateDraft(values)}
            onFinish={(values) => void createUser(values)}
            style={{ marginBottom: 24 }}
          >
            <Form.Item label="用户名" name="username">
              <Input autoComplete="off" />
            </Form.Item>
            <Form.Item label="邮箱（可选）" name="email">
              <Input />
            </Form.Item>
            <Form.Item label="展示名（可选）" name="displayName">
              <Input />
            </Form.Item>
            <Form.Item>
              <Button
                type="primary"
                htmlType="submit"
                disabled={busy || createDraft.username.trim() === ""}
              >
                创建账号
              </Button>
            </Form.Item>
          </Form>
        </>
      )}

      <Table<AdminUser>
        rowKey="id"
        size="middle"
        loading={users.isFetching}
        dataSource={users.isError ? [] : users.data?.items ?? []}
        columns={columns}
        pagination={false}
        scroll={{ x: 1000 }}
        locale={{
          emptyText: users.isError ? "账号列表加载失败。" : page === 1 ? "还没有账号。" : "本页没有账号。",
        }}
      />
      <Flex gap={12} justify="flex-end" align="center" style={{ marginTop: 16 }}>
        <Typography.Text type="secondary">第 {page} 页</Typography.Text>
        <Pagination
          current={page}
          total={users.data?.total ?? 0}
          pageSize={users.data?.per_page ?? USER_PAGE_SIZE}
          showSizeChanger={false}
          showQuickJumper
          showTotal={total => `共 ${total} 个账号`}
          disabled={busy || users.isFetching || users.isError}
          onChange={setPage}
        />
      </Flex>
    </>
  );
}

/** 单个用户的角色分配控件：选择 + 提交，避免受控状态散落在列表里。 */
function RoleAssigner({
  username,
  roles,
  busy,
  onAssign,
}: {
  username: string;
  roles: RoleSummary[];
  busy: boolean;
  onAssign: (role: string) => void;
}) {
  const [selected, setSelected] = useState<string | undefined>(undefined);
  return (
    <Space>
      <Select
        aria-label={`为 ${username} 选择角色`}
        value={selected}
        placeholder="选择角色…"
        style={{ minWidth: 160 }}
        options={roles.map((role) => ({
          value: role.slug,
          label: `${role.name}（${role.slug}）`,
        }))}
        onChange={(value: string | undefined) => setSelected(value)}
      />
      <Button
        disabled={busy || selected === undefined}
        aria-label={`为 ${username} 添加角色`}
        onClick={() => {
          if (selected !== undefined) onAssign(selected);
          setSelected(undefined);
        }}
      >
        添加角色
      </Button>
    </Space>
  );
}
