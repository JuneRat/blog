import { Alert, App as AntdApp, Button, Form, Input, Space, Table, Typography } from "antd";
import type { TableProps } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { api } from "../api";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import type { TagSummary } from "../types";

interface Draft {
  name: string;
  slug: string;
}

const EMPTY_DRAFT: Draft = { name: "", slug: "" };

/**
 * 标签目录管理屏。
 *
 * - 目录读取对全部已登录会话开放（编辑文章需要选标签）；
 * - 创建/改名/删除需 `tag.manage`（后端判定；无权限时界面隐藏管理入口）；
 * - 删除被引用的标签会被后端拒绝（409 `tag_in_use`，含草稿/私密引用），
 *   错误文案直接来自服务端（带引用规模），不前端猜测。
 *
 * 提示沿用**内联 Alert 而不是 message 吐司**：冲突与权限文案需要停留在屏幕上
 * （例如「标签仍被 3 篇引用」），3 秒后自动消失会让人来不及看清。
 */
export function TagListScreen() {
  const { me } = useAuth();
  const { modal } = AntdApp.useApp();
  /**
   * 目录走 React Query：跨屏共享缓存，写操作后用失效重取而不是自己管重载。
   *
   * 错误分两处：`tags.error` 是**取数失败**（含权限口径文案），
   * `actionError` 是前端校验与写操作失败；展示时动作错误优先。
   */
  const tags = useQuery({ queryKey: queryKeys.tags(), queryFn: () => api.listTags() });
  const queryClient = useQueryClient();
  const [actionError, setActionError] = useState<string | null>(null);
  const errorText =
    actionError ?? (tags.error === null ? null : permissionMessageOf(tags.error));
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [form] = Form.useForm<Draft>();
  /** 正在改名的标签：editingId + 编辑值。 */
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editingName, setEditingName] = useState("");

  const canManage = me?.permissions.includes("tag.manage") ?? false;
  const setError = setActionError;

  /** 写操作后让目录失效重取（替代手写的「再拉一次」）。 */
  const load = () => queryClient.invalidateQueries({ queryKey: queryKeys.tags() });

  async function create(): Promise<void> {
    const draft = form.getFieldsValue();
    const name = (draft.name ?? "").trim();
    const slug = (draft.slug ?? "").trim();
    if (name.length === 0 || slug.length === 0) {
      setError("名称与 slug 都不能为空。");
      return;
    }
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      await api.createTag({ name, slug });
      form.setFieldsValue(EMPTY_DRAFT);
      setNotice(`已创建标签 ${name}。slug 创建后不可修改。`);
      await load();
    } catch (e) {
      setError(permissionMessageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function rename(tag: TagSummary): Promise<void> {
    const name = editingName.trim();
    if (name.length === 0) {
      setError("名称不能为空。");
      return;
    }
    if (name === tag.name) {
      setEditingId(null);
      return;
    }
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      const updated = await api.renameTag(tag.slug, {
        name,
        expected_version: tag.version,
      });
      setNotice(`已改名；新版本 v${updated.version}，全部引用文章同步更新。`);
      setEditingId(null);
      await load();
    } catch (e) {
      // 版本冲突：目录已在别处被改。先重载目录（load 会清错误位），再展示冲突。
      setEditingId(null);
      await load();
      setError(permissionMessageOf(e));
    } finally {
      setBusy(false);
    }
  }

  function remove(tag: TagSummary): void {
    // 确认按钮用默认文案「确定」：行内已有「删除」按钮，同名会让定位产生歧义。
    modal.confirm({
      title: `删除标签「${tag.name}」？`,
      content: `地址 /tags/${tag.slug} 将不再可用，且不可恢复。`,
      okButtonProps: { danger: true },
      onOk: async () => {
        setError(null);
        setNotice(null);
        setBusy(true);
        try {
          await api.deleteTag(tag.slug, tag.version);
          setNotice(`已删除标签 ${tag.name}。`);
          await load();
        } catch (e) {
          // tag_in_use 的服务端文案自带引用规模；直接展示，不掩盖为通用错误。
          setError(permissionMessageOf(e));
        } finally {
          setBusy(false);
        }
      },
    });
  }

  const columns: TableProps<TagSummary>["columns"] = [
    {
      title: "名称",
      dataIndex: "name",
      render: (_value, tag) =>
        editingId === tag.id ? (
          <Input
            value={editingName}
            autoFocus
            onChange={(event) => setEditingName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") void rename(tag);
              if (event.key === "Escape") setEditingId(null);
            }}
          />
        ) : (
          <Typography.Link href={`/tags/${encodeURIComponent(tag.slug)}`} target="_blank">
            {tag.name}
          </Typography.Link>
        ),
    },
    {
      title: "slug",
      dataIndex: "slug",
      render: (slug: string) => <Typography.Text code>{slug}</Typography.Text>,
    },
    { title: "公开文章", dataIndex: "public_post_count" },
    {
      title: "版本",
      dataIndex: "version",
      render: (version: number) => <Typography.Text type="secondary">v{version}</Typography.Text>,
    },
  ];
  if (canManage) {
    columns.push({
      title: "操作",
      key: "actions",
      render: (_value, tag) =>
        editingId === tag.id ? (
          <Space>
            <Button disabled={busy} onClick={() => void rename(tag)}>
              保存
            </Button>
            <Button type="text" disabled={busy} onClick={() => setEditingId(null)}>
              取消
            </Button>
          </Space>
        ) : (
          <Space>
            <Button
              type="text"
              disabled={busy}
              onClick={() => {
                setEditingId(tag.id);
                setEditingName(tag.name);
              }}
            >
              改名
            </Button>
            <Button type="text" danger disabled={busy} onClick={() => remove(tag)}>
              删除
            </Button>
          </Space>
        ),
    });
  }

  return (
    <>
      <Typography.Title level={3}>标签</Typography.Title>

      {errorText !== null && (
        <Alert type="error" showIcon title={errorText} style={{ marginBottom: 16 }} />
      )}
      {notice !== null && (
        <Alert type="success" showIcon title={notice} style={{ marginBottom: 16 }} />
      )}

      {canManage && (
        <Form
          form={form}
          layout="inline"
          initialValues={EMPTY_DRAFT}
          onFinish={() => void create()}
          style={{ marginBottom: 24 }}
        >
          <Form.Item label="名称" name="name">
            <Input placeholder="如：Rust" />
          </Form.Item>
          <Form.Item label="slug" name="slug">
            <Input placeholder="如：rust（创建后不可改）" />
          </Form.Item>
          <Form.Item>
            <Button type="primary" htmlType="submit" disabled={busy}>
              创建标签
            </Button>
          </Form.Item>
        </Form>
      )}

      <Table<TagSummary>
        rowKey="id"
        size="middle"
        loading={tags.isPending}
        dataSource={tags.data ?? []}
        columns={columns}
        pagination={false}
        locale={{
          emptyText:
            errorText !== null
              ? "目录加载失败。"
              : canManage
                ? "还没有标签，在上方创建第一个。"
                : "还没有标签。需要持有标签管理权限的用户创建。",
        }}
      />
    </>
  );
}
