import { Alert, Button, Space, Table, Typography } from "antd";

export interface ConflictField { key: string; label: string; local: string; server: string }

const FIELD_LABELS: Record<string, string> = {
  slug: "公开地址", title: "标题", content: "正文", visibility: "可见性", excerpt: "摘要",
  tagIds: "标签", categoryId: "分类", seriesIds: "系列", seriesPositions: "系列权重", coverMediaId: "封面",
};

export function conflictFields<T extends object>(local: T, server: T | null): ConflictField[] {
  if (server === null) return [];
  const display = (value: unknown) => typeof value === "string" ? value : value === null ? "（无）" : JSON.stringify(value);
  return (Object.keys(local) as (keyof T)[])
    .filter(key => JSON.stringify(local[key]) !== JSON.stringify(server[key]))
    .map(key => ({ key: String(key), label: FIELD_LABELS[String(key)] ?? String(key), local: display(local[key]), server: display(server[key]) }));
}

export function ContentConflict({ fields, version, busy, loading, error, onRefresh, onReload, onOverwrite }: {
  fields: ConflictField[];
  version: number | null;
  busy: boolean;
  loading: boolean;
  error: string | null;
  onRefresh: () => void;
  onReload: () => void;
  onOverwrite: () => void;
}) {
  return <Alert type="warning" showIcon title="内容已在别处修改。" style={{ marginBottom: 16 }}
    description={<Space orientation="vertical" style={{ width: "100%" }}>
      <Typography.Text>你的编辑仍保留在下面，未被自动覆盖。请对比后处理；覆盖只针对此处展示的服务器版本。</Typography.Text>
      {loading && <Typography.Text>正在读取服务器版本…</Typography.Text>}
      {error && <Alert type="error" title={error} />}
      {version !== null && <>
        <Typography.Text>服务器版本 v{version}</Typography.Text>
        <Table<ConflictField> size="small" pagination={false} rowKey="key" dataSource={fields}
          scroll={{ x: 560 }} locale={{ emptyText: "可编辑字段相同，状态或关联版本可能已变化。" }}
          columns={[
            { title: "字段", dataIndex: "label", width: 90 },
            ...([['local', '本地输入'], ['server', '服务器内容']] as const).map(([key, title]) => ({
              title, dataIndex: key, render: (value: string) => <pre style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere", maxHeight: 220, overflow: "auto", margin: 0 }}>{value || "（空）"}</pre>,
            })),
          ]} />
      </>}
      <Space wrap>
        <Button disabled={busy || loading} onClick={onRefresh}>刷新对比</Button>
        <Button disabled={busy || loading || version === null} onClick={onReload}>重新加载（丢弃本地改动）</Button>
        <Button danger disabled={busy || loading || version === null} onClick={onOverwrite}>仍然覆盖</Button>
      </Space>
    </Space>} />;
}
