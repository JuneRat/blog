import { Alert, Button, Card, Empty, Flex, Image, Input, Spin, Typography } from "antd";
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { mediaPageQuery } from "../mediaQueries";
import { formatBytes } from "../media";
import { permissionMessageOf } from "../apiError";
import type { MediaAsset } from "../types";
import { ContentPagination } from "./ContentListControls";

/** 插图和封面共用文件名搜索与服务端分页，翻页不会丢失编辑器的正文或替代文字。 */
export function MediaBrowser({ onChoose, actionLabel, selectedId, disabled = false, emptyDescription }: {
  onChoose: (asset: MediaAsset) => void;
  actionLabel: string;
  selectedId?: string | null;
  disabled?: boolean;
  emptyDescription: string;
}) {
  const [filter, setFilter] = useState({ page: 1, q: "" });
  const query = useQuery({ ...mediaPageQuery(filter.page, false, filter.q), staleTime: 0 });
  return <Flex vertical gap={12}>
    <Input.Search aria-label="搜索图片" placeholder="按文件名搜索全部图片…" maxLength={200}
      allowClear enterButton="搜索" onSearch={q => setFilter({ page: 1, q: q.trim() })}
      onKeyDown={event => { if (event.key === "Enter") event.preventDefault(); }} />
    {query.error && <Alert type="error" showIcon title={permissionMessageOf(query.error)}
      action={<Button onClick={() => void query.refetch()}>重试</Button>} />}
    {query.isPending && <Flex justify="center" align="center" gap={8} style={{ padding: 12 }}>
      <Spin size="small" /><Typography.Text type="secondary">正在加载媒体库…</Typography.Text>
    </Flex>}
    {query.data?.items.length === 0 && !query.error && <Empty image={Empty.PRESENTED_IMAGE_SIMPLE}
      description={filter.q ? "没有找到匹配的图片。" : emptyDescription} />}
    <Flex wrap gap={12}>
      {query.data?.items.map(asset => <Card key={asset.id} size="small" style={{ width: 180 }}>
        <Flex vertical gap={8}>
          <Image src={asset.url} alt={asset.original_name} height={96} style={{ objectFit: "cover" }} />
          <Typography.Text code title={asset.original_name}>{asset.original_name}</Typography.Text>
          <Typography.Text type="secondary">{asset.width}×{asset.height} · {formatBytes(asset.byte_size)}</Typography.Text>
          <Button disabled={disabled || asset.id === selectedId} onClick={() => onChoose(asset)}>
            {asset.id === selectedId ? "当前图片" : actionLabel}
          </Button>
        </Flex>
      </Card>)}
    </Flex>
    <ContentPagination data={query.data} busy={query.isFetching} onChange={page => setFilter({ ...filter, page })} />
  </Flex>;
}
