import { useId } from "react";
import { Alert, Flex, Input, Select } from "antd";
import { useQuery } from "@tanstack/react-query";
import { categoryApi } from "../api/taxonomy";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import { permissionMessageOf } from "../apiError";
import type { ContentListFilter } from "../types";

export function PostScopeFilters({ filter, onChange }: {
  filter: ContentListFilter;
  onChange: (filter: Omit<ContentListFilter, "page">) => void;
}) {
  const id = useId();
  const { me } = useAuth();
  const canReadAny = me?.permissions.includes("post.read_any") ?? false;
  const categories = useQuery({ queryKey: queryKeys.categories(), queryFn: () => categoryApi.list() });
  return <>
    <Flex gap={12} wrap style={{ marginBottom: 12 }}>
      {canReadAny && <Select id={`${id}-scope`} aria-label="文章范围" value={filter.scope ?? (filter.author ? "all" : "mine")} style={{ width: 150 }}
        options={[{ value: "mine", label: "我的文章" }, { value: "all", label: "全部文章" }]}
        onChange={scope => onChange({ ...filter, scope, author: undefined })} />}
      {canReadAny && (filter.scope === "all" || filter.author) &&
        <Input.Search id={`${id}-author`} key={filter.author ?? ""} aria-label="筛选作者" placeholder="作者用户名（含停用账号）"
          defaultValue={filter.author ?? ""} allowClear style={{ width: 280 }} enterButton="筛选作者"
          onSearch={author => onChange({ ...filter, scope: "all", author: author.trim() || undefined })} />}
      <Select id={`${id}-category`} aria-label="筛选分类" placeholder="全部分类" value={filter.category_id} allowClear style={{ minWidth: 150 }}
        loading={categories.isFetching} options={(categories.data ?? []).map(item => ({ value: item.id, label: item.name }))}
        onChange={category_id => onChange({ ...filter, category_id })} />
    </Flex>
    {categories.error && <Alert type="error" title={permissionMessageOf(categories.error)} style={{ marginBottom: 12 }} />}
  </>;
}
