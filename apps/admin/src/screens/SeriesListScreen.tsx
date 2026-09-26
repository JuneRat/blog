import { statusLabel } from "../components/ContentLifecycleControls";
import {
  Alert,
  App as AntdApp,
  Button,
  Form,
  Input,
  Modal,
  Space,
  Table,
  Typography,
} from "antd";
import type { TableProps } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import type { ReactNode } from "react";
import { seriesApi } from "../api";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { CoverPicker } from "../components/CoverPicker";
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
  /**
   * 正在编辑封面的系列（null = 弹窗关闭）与弹窗里的选择值。
   *
   * 选择值单独存一份而不是直接读 `coverEditing`：弹窗内改动在点「保存」前
   * 不应影响表格；保存成功/失败后再由目录刷新覆盖。
   */
  const [coverEditing, setCoverEditing] = useState<SeriesSummary | null>(null);
  const [coverValue, setCoverValue] = useState<string | null>(null);
  const canManage = me?.permissions.includes("series.manage") ?? false;
  const canReadMedia = me?.permissions.includes("media.read") ?? false;
  const canUploadMedia = me?.permissions.includes("media.upload") ?? false;
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
              (a, b) => a.position - b.position || a.id.localeCompare(b.id),
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
      content: "只移除目录和文章的关联，文章内容会保留。",
      okButtonProps: { danger: true },
      onOk: async () => {
        setError(null);
        setNotice(null);
        setBusy(true);
        try {
          await seriesApi.remove(s.slug, s.version);
          void queryClient.invalidateQueries({queryKey: queryKeys.posts()});
          void queryClient.invalidateQueries({queryKey: queryKeys.trashAll()});
          setNotice(`已删除系列 ${s.name}。`);
          await load();
        } catch (e) {
          setError(permissionMessageOf(e));
        } finally {
          setBusy(false);
        }
      },
    });
  }

  /** 打开封面弹窗：以目录里的当前封面为起点，取消则不改动任何数据。 */
  function editCover(s: SeriesSummary): void {
    setError(null);
    setNotice(null);
    setCoverValue(s.cover_media_id);
    setCoverEditing(s);
  }

  /**
   * 保存封面。
   *
   * 系列更新接口的 `name` 必填，因此改封面也必须原样带上名称与描述；
   * `cover_media_id` 按绝对值提交（null = 移除，id = 设置）。
   * 失败时先重读目录（拿到最新 version 与封面）再展示服务端原因，
   * 并关闭弹窗——内联 Alert 在弹窗遮罩后面看不见，且旧 version 重试也不会成功。
   */
  async function saveCover(): Promise<void> {
    const s = coverEditing;
    if (s === null) return;
    setError(null);
    setNotice(null);
    setBusy(true);
    try {
      await seriesApi.update(s.slug, {
        name: s.name,
        description: s.description ?? undefined,
        cover_media_id: coverValue,
        expected_version: s.version,
      });
      setCoverEditing(null);
      setNotice(`已更新系列 ${s.name} 的封面。`);
      await load();
    } catch (e) {
      // 版本冲突/越权同样先重读目录，再展示服务端文案（与重排失败一致）。
      await load();
      setCoverEditing(null);
      setError(permissionMessageOf(e));
    } finally {
      setBusy(false);
    }
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
      void queryClient.invalidateQueries({queryKey: queryKeys.posts()});
      void queryClient.invalidateQueries({queryKey: queryKeys.trashAll()});
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
              <Typography.Link href={`/admin/posts/${encodeURIComponent(post.id)}/edit`}>
                {post.title || post.slug}
              </Typography.Link>
            )}
            <Typography.Text type="secondary">
              {post.deleted ? "回收站 · " : ""}
              {statusLabel(post.status)}
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
    {
      title: "封面",
      key: "cover",
      width: 112,
      render: (_value, s) =>
        s.cover_url === null ? (
          <Typography.Text type="secondary">无封面</Typography.Text>
        ) : (
          // 用原生 img 而不是 antd Image：列表里只需要缩略图，不需要预览弹层。
          <img
            src={s.cover_url}
            alt={`${s.name} 的封面`}
            style={{ width: 72, height: 40, objectFit: "cover", borderRadius: 4 }}
          />
        ),
    },
  ];
  if (canManage) {
    columns.push({
      title: "操作",
      key: "actions",
      render: (_value, s) => (
        <Space size={0}>
          <Button type="text" disabled={busy} onClick={() => editCover(s)}>
            封面
          </Button>
          <Button type="text" danger disabled={busy} onClick={() => remove(s)}>
            删除
          </Button>
        </Space>
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

      {/*
        只挂载打开的弹窗：关闭即卸载，避免残留的选择器列表与文件输入影响
        后续交互（与「关闭后不保留上次列表」的选择器语义一致）。
      */}
      {coverEditing !== null && (
        <Modal
          title={`设置系列「${coverEditing.name}」的封面`}
          open
          okText="保存"
          cancelText="取消"
          confirmLoading={busy}
          onOk={() => void saveCover()}
          onCancel={() => setCoverEditing(null)}
          destroyOnHidden
        >
          <CoverPicker
            value={coverValue}
            onChange={setCoverValue}
            // 后端下发的 cover_url 只对「当前目录里的那一个 id」有效：一旦在弹窗里
            // 换了封面，value 已不是它，必须回退到 mediaUrl(value) 显示新图，
            // 否则预览会一直停在旧封面上。
            currentUrl={
              coverValue === coverEditing.cover_media_id ? coverEditing.cover_url : null
            }
            canReadMedia={canReadMedia}
            canUploadMedia={canUploadMedia}
            disabled={busy}
            label="系列封面"
          />
        </Modal>
      )}
    </>
  );
}
