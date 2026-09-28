import { Button, Flex, Select, Typography } from "antd";
import { useId } from "react";
import type { ContentListFilter, ContentPage, Visibility } from "../types";
import { statusLabel } from "./ContentLifecycleControls";

export function ContentListFilters({ filter, onChange }: {
  filter: ContentListFilter;
  onChange: (filter: Omit<ContentListFilter, "page">) => void;
}) {
  const id = useId();
  return <Flex gap={12} wrap style={{ marginBottom: 16 }}>
    <Select id={`${id}-status`} aria-label="筛选状态" style={{ minWidth: 140 }} value={filter.status ?? ""}
      options={[{ value: "", label: "全部状态" }, ...["draft", "scheduled", "published", "archived"].map(value => ({ value, label: statusLabel(value) }))]}
      onChange={status => onChange({ status: status || undefined, visibility: filter.visibility })} />
    <Select<Visibility | ""> id={`${id}-visibility`} aria-label="筛选可见性" style={{ minWidth: 140 }} value={filter.visibility ?? ""}
      options={[{ value: "", label: "全部可见性" }, { value: "public", label: "公开" }, { value: "private", label: "私有" }]}
      onChange={visibility => onChange({ status: filter.status, visibility: visibility || undefined })} />
  </Flex>;
}

export function ContentPagination({ data, busy, onChange }: {
  data: Pick<ContentPage<unknown>, "page" | "per_page" | "total"> | undefined;
  busy: boolean;
  onChange: (page: number) => void;
}) {
  if (!data) return null;
  return <Flex gap={12} align="center" style={{ marginTop: 16 }}>
    <Button disabled={busy || data.page <= 1} onClick={() => onChange(data.page - 1)}>上一页</Button>
    <Typography.Text>第 {data.page} 页，共 {data.total} 条</Typography.Text>
    <Button disabled={busy || data.page * data.per_page >= data.total} onClick={() => onChange(data.page + 1)}>下一页</Button>
  </Flex>;
}
