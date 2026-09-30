import { Alert, Checkbox, Form, Input, Select, Typography } from "antd";
import type { FormInstance } from "antd";
import { useQuery } from "@tanstack/react-query";
import { tagsApi, categoryApi, seriesApi } from "../../api/taxonomy";
import { queryKeys } from "../../queryClient";
import { permissionMessageOf } from "../../apiError";
import { navigate, paths } from "../../router";
import { useLeaveConfirmation } from "../../unsaved";
import { CoverPicker } from "../../components/CoverPicker";
import type { FormState } from "./form";

export function PostMetadataFields({
  form,
  view,
  uploadScope,
  canReadMedia,
  canUploadMedia,
  hideTitle = false,
}: {
  form: FormInstance<FormState>;
  view: FormState;
  uploadScope: string;
  canReadMedia: boolean;
  canUploadMedia: boolean;
  hideTitle?: boolean;
}) {
  const confirmLeave = useLeaveConfirmation();
  /**
   * 三个目录（标签/分类/系列）走 React Query：与标签、分类、系列三个管理屏
   * 共用同一份缓存，编辑器之间也不再各拉一遍。
   *
   * 目录加载失败**不阻塞正文编辑**——只是暂时无法勾选，所以这里不进入 loading 分支，
   * 只在选择区上方显示一条错误。
   */
  const tagsQuery = useQuery({
    queryKey: queryKeys.tags(),
    queryFn: () => tagsApi.listTags(),
  });
  const categoriesQuery = useQuery({
    queryKey: queryKeys.categories(),
    queryFn: () => categoryApi.list(),
  });
  const seriesQuery = useQuery({
    queryKey: queryKeys.series(),
    queryFn: () => seriesApi.list(),
  });
  const catalog = tagsQuery.data ?? null;
  const categoryCatalog = categoriesQuery.data ?? null;
  const seriesCatalog = seriesQuery.data ?? null;
  const catalogFailure =
    tagsQuery.error ?? categoriesQuery.error ?? seriesQuery.error;
  const catalogError =
    catalogFailure === null ? null : permissionMessageOf(catalogFailure);
  return (
    <>
      <Form.Item label="slug" name="slug">
        <Input placeholder="留空则自动生成（预约或发布后锁定）" />
      </Form.Item>
      {!hideTitle && (
        <Form.Item label="标题" name="title">
          <Input />
        </Form.Item>
      )}
      <Form.Item label="摘要" name="excerpt">
        <Input placeholder="文章简短摘要（可选）" />
      </Form.Item>
      <Form.Item name="coverMediaId" hidden>
        <Input />
      </Form.Item>
      <CoverPicker
        uploadScope={uploadScope}
        value={view.coverMediaId}
        onChange={(id) => form.setFieldValue("coverMediaId", id)}
        canReadMedia={canReadMedia}
        canUploadMedia={canUploadMedia}
      />
      <Form.Item label="可见性" name="visibility">
        <Select
          options={[
            { value: "public", label: "公开" },
            { value: "private", label: "私有" },
          ]}
        />
      </Form.Item>
      <Form.Item label="分类" name="categoryId">
        <Select
          allowClear
          placeholder="（未分类）"
          options={(categoryCatalog ?? []).map((category) => ({
            value: category.id,
            label: category.name,
          }))}
        />
      </Form.Item>
      <Form.Item label="系列" name="seriesIds">
        <Select
          mode="multiple"
          allowClear
          placeholder="可加入多个系列"
          options={(seriesCatalog ?? []).map((series) => ({
            value: series.id,
            label: series.name,
          }))}
        />
      </Form.Item>
      {view.seriesIds.map((seriesId) => (
        <Form.Item
          key={seriesId}
          label={`${seriesCatalog?.find((s) => s.id === seriesId)?.name ?? seriesId} · 排序权重`}
          name={["seriesPositions", seriesId]}
          initialValue="0"
        >
          <Input
            inputMode="numeric"
            placeholder="0 起；数值越小越靠前，允许重复"
          />
        </Form.Item>
      ))}
      <Form.Item label="标签" name="tagIds">
        <Checkbox.Group
          options={(catalog ?? []).map((tag) => ({
            label: tag.name,
            value: tag.id,
          }))}
        />
      </Form.Item>
      {catalogError !== null && (
        <Alert
          type="error"
          showIcon
          title={catalogError}
          style={{ marginBottom: 16 }}
        />
      )}
      {catalog === null && catalogError === null && (
        <Typography.Text
          type="secondary"
          style={{ display: "block", marginBottom: 16 }}
        >
          正在加载标签目录…
        </Typography.Text>
      )}
      {catalog !== null && catalog.length === 0 && (
        <Typography.Text
          type="secondary"
          style={{ display: "block", marginBottom: 16 }}
        >
          还没有可用标签；先在
          <Typography.Link
            href={paths.tags}
            onClick={(event) => {
              event.preventDefault();
              // 屏内离页入口走同一确认口径，否则保护只覆盖侧栏菜单。
              confirmLeave(() => navigate(paths.tags));
            }}
          >
            标签目录
          </Typography.Link>
          创建。
        </Typography.Text>
      )}
    </>
  );
}
