import { Alert, App as AntdApp, Button, Form, Input, Space, Table, Typography } from "antd";
import type { TableProps } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { seriesApi } from "../api";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import type { SeriesMemberRow, SeriesSummary } from "../types";

interface Draft {
  name: string;
  slug: string;
}

const EMPTY_DRAFT: Draft = { name: "", slug: "" };

/**
 * 系列管理屏：目录 CRUD + 成员顺序调整。
 *
 * - 管理需 series.manage；目录读取开放；
 * - 成员列表来自文章列表接口（按 series 过滤在前端完成——列表接口返回
 *   全部状态，含草稿/私密，它们保留位置但公开页不出现）；
 * - 上移/下移走整体重排接口：先交换再提交完整顺序与当前 series 版本；
 *   成功后用响应版本继续；403/409 原样展示（Author 不能重排他人文章）。
 *
 * 提示沿用**内联 Alert 而不是 message 吐司**：版本冲突、越权与「成员目录不可读」
 * 需要停留在屏幕上（后者还带重排所需的权限说明），3 秒后自动消失会来不及看清。
 */
export function SeriesListScreen() {
  const { me } = useAuth();
  const { modal } = AntdApp.useApp();
  /** 目录列表走 React Query：与两个编辑器共用同一份缓存，写操作后失效重取。 */
  const series = useQuery({ queryKey: queryKeys.series(), queryFn: () => seriesApi.list() });
  const queryClient = useQueryClient();
  const [actionError, setActionError] = useState<string | null>(null);
  const errorText =
    actionError ?? (series.error === null ? null : permissionMessageOf(series.error));
  const [members, setMembers] = useState<Record<string, SeriesMemberRow[]>>({});
  /** 成员目录不可读的系列（id → 原因）：显示权限提示并禁用重排，
   * 不当成空目录——空目录会误导「还没有文章加入」，也删掉了重排入口。 */
  const [unreadable, setUnreadable] = useState<Record<string, string>>({});
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [form] = Form.useForm<Draft>();
  /** 展开的系列行：成员与系列同屏可见（与原「每个系列一段」一致），
   * 但展开键必须在目录到达后才能设上——Table 的 defaultExpandAllRows
   * 只在首帧生效，而首帧 dataSource 还是空的。 */
  const [expandedKeys, setExpandedKeys] = useState<string[]>([]);
  const canManage = me?.permissions.includes("series.manage") ?? false;
  // 屏内多处直接设置文案（如「名称与 slug 都不能为空。」），沿用同一个 setter。
  const setError = setActionError;

  /** 写操作后让目录失效重取（替代手写「再拉一次」）。 */
  const load = () => queryClient.invalidateQueries({ queryKey: queryKeys.series() });

  /**
   * 成员目录：外层列表就绪后按系列并发拉取。
   *
   * 保留逐系列独立收尾——任一 403 不得连带清空其它系列（混合系列含他人文章时
   * 对本用户不可读是正常状态），所以这里没有把成员也交给单个查询。
   */
  useEffect(() => {
    const current = series.data;
    if (current === undefined) {
      // 目录本身失败：只清展开键，成员与不可读状态保留旧值。
      if (series.error !== null) setExpandedKeys([]);
      return;
    }
    setExpandedKeys(current.map((s) => s.id));
    void (async () => {
      // 成员目录逐系列处理：任一 403 不得连带清空其它系列——
      // 混合系列（含他人文章）对本用户不可读是**正常状态**，
      // 它不该让可管理的独著系列一起消失。
      const bySeries: Record<string, SeriesMemberRow[]> = {};
      const blocked: Record<string, string> = {};
      await Promise.all(
        current.map(async (s) => {
          try {
            // 成员走专用目录端点：包含其他作者的成员（重排会改动它们的位置，
            // 无参 listPosts 只回当前作者的文章，多人系列会缺员）。
            const rows = await seriesApi.members(s.slug);
            bySeries[s.id] = [...rows].sort(
              (a, b) => (a.series_order ?? 0) - (b.series_order ?? 0),
            );
          } catch (e) {
            blocked[s.id] = permissionMessageOf(e);
          }
        }),
      );
      setMembers(bySeries);
      setUnreadable(blocked);
    })();
  }, [series.data, series.error]);

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
      await seriesApi.create({ name, slug });
      form.setFieldsValue(EMPTY_DRAFT);
      setNotice(`已创建系列 ${name}。在文章编辑器中把文章加入系列。`);
      await load();
    } catch (e) {
      setError(permissionMessageOf(e));
    } finally {
      setBusy(false);
    }
  }

  function remove(s: SeriesSummary): void {
    // 确认按钮用默认文案「确定」：行内已有「删除」按钮，同名会让定位产生歧义。
    modal.confirm({
      title: `删除系列「${s.name}」（/series/${s.slug}）？`,
      okButtonProps: { danger: true },
      onOk: async () => {
        setError(null);
        setNotice(null);
        setBusy(true);
        try {
          await seriesApi.remove(s.slug, s.version);
          setNotice(`已删除系列 ${s.name}。`);
          await load();
        } catch (e) {
          // series_in_use 的服务端文案自带引用规模；直接展示。
          setError(permissionMessageOf(e));
        } finally {
          setBusy(false);
        }
      },
    });
  }

  /** 与相邻成员交换后提交完整顺序。 */
  async function move(s: SeriesSummary, index: number, delta: -1 | 1): Promise<void> {
    const list = members[s.id] ?? [];
    const target = index + delta;
    if (target < 0 || target >= list.length) return;
    const next = [...list];
    [next[index], next[target]] = [next[target], next[index]];
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      await seriesApi.reorder(
        s.slug,
        next.map((p) => p.id),
        s.version,
      );
      await load();
    } catch (e) {
      // 版本/权限/集合不一致：先重读目录（load 清错误位），再展示服务端原因。
      await load();
      setError(permissionMessageOf(e));
    } finally {
      setBusy(false);
    }
  }

  /** 一个系列的成员区：不可读 → 权限提示；空 → 空态文案；否则可重排的有序列表。 */
  function membersOf(s: SeriesSummary): ReactNode {
    const blocked = unreadable[s.id];
    if (blocked !== undefined) {
      // 不当成空目录：空目录会误导「还没有文章加入」，也删掉了重排入口。
      return (
        <Alert
          type="error"
          showIcon
          title={`成员目录不可读：${blocked}（重排需要全部成员的读取权限；他人文章所在系列对无 post.read_any 的调用者不可见。）`}
        />
      );
    }
    const list = members[s.id] ?? [];
    if (list.length === 0) {
      return <Typography.Text type="secondary">还没有文章加入这个系列。</Typography.Text>;
    }
    const memberColumns: TableProps<SeriesMemberRow>["columns"] = [
      {
        title: "序号",
        key: "order",
        width: 64,
        render: (_value, _post, index) => (
          <Typography.Text type="secondary">{index + 1}</Typography.Text>
        ),
      },
      {
        title: "文章",
        dataIndex: "title",
        render: (_value, post) => (
          <Space size={8}>
            {post.deleted ? (
              <Typography.Text>{post.title || post.slug}</Typography.Text>
            ) : (
              <Typography.Link href={`/admin/posts/${encodeURIComponent(post.slug)}/edit`}>
                {post.title || post.slug}
              </Typography.Link>
            )}
            <Typography.Text type="secondary">
              {post.deleted ? "回收站 · " : ""}
              {post.status === "published" ? "已发布" : post.status === "archived" ? "已归档" : "草稿"}
              {post.author_id !== me?.user_id ? " · 他人文章" : ""}
            </Typography.Text>
          </Space>
        ),
      },
    ];
    if (canManage) {
      memberColumns.push({
        title: "操作",
        key: "actions",
        width: 96,
        render: (_value, _post, index) => (
          <Space size={0}>
            <Button
              type="text"
              disabled={busy || index === 0}
              onClick={() => void move(s, index, -1)}
            >
              ↑
            </Button>
            <Button
              type="text"
              disabled={busy || index === list.length - 1}
              onClick={() => void move(s, index, 1)}
            >
              ↓
            </Button>
          </Space>
        ),
      });
    }
    return (
      <Table<SeriesMemberRow>
        rowKey="id"
        size="small"
        showHeader={false}
        dataSource={list}
        columns={memberColumns}
        pagination={false}
      />
    );
  }

  const columns: TableProps<SeriesSummary>["columns"] = [
    {
      title: "系列",
      dataIndex: "name",
      render: (_value, s) => (
        <Space size={8}>
          <Typography.Link
            href={`/series/${encodeURIComponent(s.slug)}`}
            target="_blank"
            rel="noreferrer"
          >
            {s.name}
          </Typography.Link>
          <Typography.Text type="secondary">
            {s.pub_post_count}/{s.post_count} 篇公开 · v{s.version}
          </Typography.Text>
        </Space>
      ),
    },
  ];
  if (canManage) {
    columns.push({
      title: "操作",
      key: "actions",
      render: (_value, s) => (
        <Button type="text" danger disabled={busy} onClick={() => remove(s)}>
          删除
        </Button>
      ),
    });
  }

  return (
    <>
      <Typography.Title level={3}>系列</Typography.Title>

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
            <Input placeholder="如：Rust 入门系列" />
          </Form.Item>
          <Form.Item label="slug" name="slug">
            <Input placeholder="如：rust-intro（创建后不可改）" />
          </Form.Item>
          <Form.Item>
            <Button type="primary" htmlType="submit" disabled={busy}>
              创建系列
            </Button>
          </Form.Item>
        </Form>
      )}

      <Table<SeriesSummary>
        rowKey="id"
        size="middle"
        loading={series.isPending}
        dataSource={series.data ?? []}
        columns={columns}
        pagination={false}
        expandable={{
          expandedRowKeys: expandedKeys,
          onExpandedRowsChange: (keys) => setExpandedKeys([...keys] as string[]),
          expandedRowRender: (s) => membersOf(s),
        }}
        locale={{
          emptyText:
            errorText !== null
              ? "目录加载失败。"
              : canManage
                ? "还没有系列。在下方创建第一个。"
                : "还没有系列。需要持有系列管理权限的用户创建。",
        }}
      />
    </>
  );
}
