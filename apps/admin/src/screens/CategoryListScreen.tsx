import { invalidateAfterWrite } from "../queryEffects";
import { Alert, App as AntdApp, Button, Form, Input, Select, Space, Table, Typography } from "antd";
import type { TableProps } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { categoryApi } from "../api/taxonomy";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import type { CategorySummary } from "../types";

interface Draft {
  name: string;
  slug: string;
  parent: string;
}

const EMPTY_DRAFT: Draft = { name: "", slug: "", parent: "" };

/**
 * 分类目录管理屏（树形缩进展示；管理需 category.manage，后端判定）。
 * 移动成环、删除保护（被引用/有子分类）的错误文案来自服务端。
 *
 * 提示沿用**内联 Alert 而不是 message 吐司**：成环、被引用规模与权限文案
 * 需要停留在屏幕上（例如「分类仍被 2 篇文章引用、仍有 1 个子分类」），
 * 3 秒后自动消失会让人来不及看清。
 */
export function CategoryListScreen() {
  const { me } = useAuth();
  const { modal } = AntdApp.useApp();
  /**
   * 目录走 React Query：屏内「父分类」「移动到…」两处下拉与表格共用同一份缓存，
   * 写操作后用失效重取，而不是自己管重载。
   *
   * 错误分两处：`categories.error` 是**取数失败**（含权限口径文案），
   * `actionError` 是前端校验与写操作失败；展示时动作错误优先。
   */
  const categories = useQuery({ queryKey: queryKeys.categories(), queryFn: () => categoryApi.list() });
  const queryClient = useQueryClient();
  const [actionError, setActionError] = useState<string | null>(null);
  const errorText =
    actionError ?? (categories.error === null ? null : permissionMessageOf(categories.error));
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [form] = Form.useForm<Draft>();
  const canManage = me?.permissions.includes("category.manage") ?? false;
  // 屏内多处直接设置文案（如「名称与 slug 都不能为空。」），沿用同一个 setter。
  const setError = setActionError;

  /** 写操作后让目录失效重取（替代手写的「再拉一次」）。 */
  const load = () => invalidateAfterWrite(queryClient, "category");

  /** 由 parent_id 计算缩进深度（目录小，直接逐层上溯）。 */
  function depthOf(cat: CategorySummary): number {
    const byId = new Map((categories.data ?? []).map((c) => [c.id, c]));
    let depth = 0;
    let cur = cat;
    while (cur.parent_id !== null && depth < 100) {
      const parent: CategorySummary | undefined = byId.get(cur.parent_id);
      if (parent === undefined) break;
      cur = parent;
      depth += 1;
    }
    return depth;
  }

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
      const parentSlug = (draft.parent ?? "").trim();
      const parent = parentSlug.length > 0 ? parentSlug : undefined;
      await categoryApi.create({ name, slug, parent });
      form.setFieldsValue(EMPTY_DRAFT);
      setNotice(`已创建分类 ${name}。slug 创建后不可修改。`);
      await load();
    } catch (e) {
      setError(permissionMessageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function moveToRoot(cat: CategorySummary): Promise<void> {
    if (cat.parent_id === null) return;
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      await categoryApi.update(cat.slug, { name: cat.name, parent: null, expected_version: cat.version });
      setNotice(`已把「${cat.name}」移到根。`);
      await load();
    } catch (e) {
      // load 会清错误位：先重载目录，再展示服务端错误（成环/版本冲突）。
      await load();
      setError(permissionMessageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function moveUnder(cat: CategorySummary, parentSlug: string): Promise<void> {
    if (parentSlug === cat.slug) {
      setError("父分类不能是自身。");
      return;
    }
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      await categoryApi.update(cat.slug, { name: cat.name, parent: parentSlug, expected_version: cat.version });
      setNotice(`已把「${cat.name}」移到 ${parentSlug} 之下。`);
      await load();
    } catch (e) {
      // 同上：先重载再报错。
      await load();
      setError(permissionMessageOf(e));
    } finally {
      setBusy(false);
    }
  }

  function remove(cat: CategorySummary): void {
    // 确认按钮用默认文案「确定」：行内已有「删除」按钮，同名会让定位产生歧义。
    modal.confirm({
      title: `删除分类「${cat.name}」（/categories/${cat.slug}）？`,
      okButtonProps: { danger: true },
      onOk: async () => {
        setError(null);
        setNotice(null);
        setBusy(true);
        try {
          await categoryApi.remove(cat.slug, cat.version);
          setNotice(`已删除分类 ${cat.name}。`);
          await load();
        } catch (e) {
          // category_in_use 的服务端文案自带引用规模与子分类数；直接展示。
          setError(permissionMessageOf(e));
        } finally {
          setBusy(false);
        }
      },
    });
  }

  const columns: TableProps<CategorySummary>["columns"] = [
    {
      title: "名称",
      dataIndex: "name",
      render: (_value, cat) => (
        <span style={{ paddingLeft: `${depthOf(cat) * 1.25}rem` }}>
          <Typography.Link
            href={`/categories/${encodeURIComponent(cat.slug)}`}
            target="_blank"
            rel="noreferrer"
          >
            {cat.name}
          </Typography.Link>
        </span>
      ),
    },
    {
      title: "slug",
      dataIndex: "slug",
      render: (slug: string) => <Typography.Text code>{slug}</Typography.Text>,
    },
    { title: "公开文章", dataIndex: "pub_post_count" },
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
      render: (_value, cat) => (
        <Space>
          <Select
            aria-label={`移动 ${cat.name}`}
            value={null}
            placeholder="移动到…"
            disabled={busy}
            style={{ minWidth: 120 }}
            options={[
              { value: "__root", label: "（根分类）" },
              ...(categories.data ?? [])
                .filter((c) => c.id !== cat.id)
                .map((c) => ({ value: c.slug, label: c.name })),
            ]}
            onChange={(value: string) => {
              if (value === "__root") void moveToRoot(cat);
              else if (value.length > 0) void moveUnder(cat, value);
            }}
          />
          <Button type="text" danger disabled={busy} onClick={() => remove(cat)}>
            删除
          </Button>
        </Space>
      ),
    });
  }

  return (
    <>
      <Typography.Title level={3}>分类</Typography.Title>

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
            <Input placeholder="如：技术" />
          </Form.Item>
          <Form.Item label="slug" name="slug">
            <Input placeholder="如：tech（创建后不可改）" />
          </Form.Item>
          <Form.Item label="父分类" name="parent">
            <Select
              style={{ minWidth: 140 }}
              options={[
                { value: "", label: "（根分类）" },
                ...(categories.data ?? []).map((c) => ({ value: c.slug, label: c.name })),
              ]}
            />
          </Form.Item>
          <Form.Item>
            <Button type="primary" htmlType="submit" disabled={busy}>
              创建分类
            </Button>
          </Form.Item>
        </Form>
      )}

      <Table<CategorySummary>
        rowKey="id"
        size="middle"
        loading={categories.isPending}
        dataSource={categories.data ?? []}
        columns={columns}
        pagination={false}
        locale={{
          emptyText:
            errorText !== null
              ? "目录加载失败。"
              : canManage
                ? "还没有分类。在下方创建第一个。"
                : "还没有分类。需要持有分类管理权限的用户创建。",
        }}
      />
    </>
  );
}
