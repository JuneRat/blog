import { useEffect, useId, useState } from "react";
import { AutoComplete, Button, Flex, Input, Select, Typography } from "antd";
import { useQuery } from "@tanstack/react-query";
import { pagesApi } from "../api/pages";
import { queryKeys } from "../queryClient";
import type { NavigationItem } from "../api/generated";

export function NavigationEditor({ value = [], onChange, canReadPages = false }: {
  value?: NavigationItem[];
  onChange?: (items: NavigationItem[]) => void;
  canReadPages?: boolean;
}) {
  const id = useId();
  const [search, setSearch] = useState<string | null>(null);
  const [query, setQuery] = useState<string | null>(null);
  useEffect(() => {
    const timer = window.setTimeout(() => setQuery(search), 250);
    return () => window.clearTimeout(timer);
  }, [search]);
  const filter = { page: 1, q: query?.trim() || undefined };
  const pagesQuery = useQuery({
    queryKey: queryKeys.pageList(filter),
    queryFn: () => pagesApi.listPages(filter),
    enabled: canReadPages && query !== null,
    staleTime: 60_000,
  });
  const pageOptions = (canReadPages && query === search ? pagesQuery.data?.items ?? [] : []).map((page) => ({
    value: page.slug,
    label: `${page.title || page.slug} (/${page.slug})`,
  }));

  const update = (index: number, patch: Partial<NavigationItem>) =>
    onChange?.(value.map((item, i) => i === index ? { ...item, ...patch } : item));
  function move(index: number, delta: number) {
    const next = [...value];
    [next[index], next[index + delta]] = [next[index + delta], next[index]];
    onChange?.(next);
  }
  return <Flex vertical gap={12}>
    <Typography.Paragraph type="secondary">
      添加独立页面的导航，按下方顺序显示。目标填写页面 slug（如 about）。尚未公开、撤回、私有或已删除的页面自动隐藏入口，重新公开后恢复。
    </Typography.Paragraph>
    {value.map((item, index) => <Flex key={index} gap={8} wrap align="center">
      <Input aria-label={`导航 ${index + 1} 名称`} placeholder="导航名称" value={item.label}
        onChange={event => update(index, { label: event.target.value })} style={{ width: 150 }} />
      <AutoComplete aria-label={`导航 ${index + 1} 目标页面`} placeholder="页面 slug，例如 about" value={item.page_slug}
        options={pageOptions} filterOption={false}
        onFocus={() => setSearch(item.page_slug)} onBlur={() => setSearch(null)}
        onChange={text => { setSearch(text); update(index, { page_slug: text }); }} style={{ width: 220 }} />
      <Select id={`${id}-${index}-placement`} aria-label={`导航 ${index + 1} 显示位置`} value={item.placement} style={{ width: 100 }}
        options={[{ value: "header", label: "页头" }, { value: "footer", label: "页脚" }]}
        onChange={placement => update(index, { placement })} />
      <Button aria-label={`上移导航 ${index + 1}`} disabled={index === 0} onClick={() => move(index, -1)}>上移</Button>
      <Button aria-label={`下移导航 ${index + 1}`} disabled={index === value.length - 1} onClick={() => move(index, 1)}>下移</Button>
      <Button danger aria-label={`删除导航 ${index + 1}`} onClick={() => onChange?.(value.filter((_, i) => i !== index))}>删除</Button>
    </Flex>)}
    {canReadPages && search !== null && pagesQuery.isError && <Typography.Text type="warning" role="status">
      页面建议加载失败，仍可手动填写页面 slug。
    </Typography.Text>}
    <Button disabled={value.length >= 20} onClick={() => onChange?.([...value, { label: "", page_slug: "", placement: "header" }])}>添加导航</Button>
  </Flex>;
}
