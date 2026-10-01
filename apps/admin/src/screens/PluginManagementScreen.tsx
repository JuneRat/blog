import { Alert, Button, Card, Empty, Form, Input, InputNumber, Modal, Space, Switch, Tag, Typography } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { ApiError } from "../api/client";
import type { PluginConfigValue, PluginView, SavePluginInput } from "../api/generated";
import { pluginsApi } from "../api/plugins";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import { invalidateAfterWrite } from "../queryEffects";
import { useEditorRequestGuard } from "../useEditorRequestGuard";
import { useUnsavedGuard } from "../unsaved";

type Draft = { plugin: PluginView; config: Record<string, PluginConfigValue>; version: number };

export function PluginManagementScreen() {
  const { me } = useAuth();
  const allowed = me?.permissions.includes("plugins.manage") ?? false;
  const client = useQueryClient();
  const query = useQuery({ queryKey: queryKeys.plugins(), queryFn: ({ signal }) => pluginsApi.get(signal), enabled: allowed });
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const beginRequest = useEditorRequestGuard("plugin-management");
  const dirty = draft !== null && JSON.stringify(draft.config) !== JSON.stringify(draft.plugin.config);
  useUnsavedGuard(dirty, "插件配置有未保存的修改，离开会丢失。");

  async function save(plugin: PluginView, input: SavePluginInput) {
    if (busyRef.current || !allowed) return;
    const isCurrent = beginRequest();
    busyRef.current = true; setBusy(true); setError(null); setNotice(null);
    try {
      await client.cancelQueries({ queryKey: queryKeys.plugins() });
      const result = await pluginsApi.save(plugin.id, input);
      client.setQueryData(queryKeys.plugins(), result);
      void invalidateAfterWrite(client, "plugin");
      if (isCurrent()) {
        setDraft(null);
        setNotice(plugin.hooks.includes("content")
          ? "插件设置已保存。已有文章可在任务管理中重建内容后应用新的正文规则。"
          : "插件设置已保存。");
      }
    } catch (cause) {
      if (isCurrent()) setError(cause instanceof ApiError && cause.code === "version_conflict"
        ? "插件设置已被其他操作修改。当前输入已保留，请关闭配置并刷新插件列表后重试。"
        : cause instanceof ApiError ? permissionMessageOf(cause) : "无法连接服务器，请刷新插件列表确认结果后重试。");
    } finally {
      busyRef.current = false;
      if (isCurrent()) setBusy(false);
    }
  }

  function change(key: string, value: PluginConfigValue) {
    setDraft(current => current ? { ...current, config: { ...current.config, [key]: value } } : null);
  }

  if (!allowed) return <><Typography.Title level={3}>插件管理</Typography.Title><Alert type="warning" showIcon title="当前账号没有管理插件的权限。" /></>;
  const readError = query.error ? query.error instanceof ApiError ? permissionMessageOf(query.error) : "无法读取插件列表，请重试。" : null;
  const errorNotice = error ?? readError;
  return <>
    <Typography.Title level={3}>插件管理</Typography.Title>
    <Space style={{ marginBottom: 16 }} wrap>
      <Typography.Text type="secondary">管理站点扩展，启用后在对应页面生效。</Typography.Text>
      <Button disabled={busy || draft !== null || query.isFetching} onClick={() => { setError(null); setNotice(null); void query.refetch(); }}>刷新插件列表</Button>
    </Space>
    {errorNotice && !draft && <Alert type="error" showIcon title={errorNotice} style={{ marginBottom: 16 }} />}
    {notice && <Alert type="success" showIcon title={notice} style={{ marginBottom: 16 }} />}
    {query.isPending && <Typography.Paragraph role="status">正在读取插件…</Typography.Paragraph>}
    {query.data?.plugins.length === 0 && <Empty description="暂无可用插件"><Typography.Text type="secondary">后续接入的插件会在这里显示。</Typography.Text></Empty>}
    <Space orientation="vertical" style={{ width: "100%" }} size="middle">
      {query.data?.plugins.map(plugin => <Card key={plugin.id} title={<Space>{plugin.name}{plugin.version && <Tag>{plugin.version}</Tag>}</Space>}
        extra={<Switch aria-label={`启用 ${plugin.name}`} checked={plugin.enabled}
          disabled={busy || draft !== null || query.isFetching || query.isError || (!plugin.available && !plugin.enabled)}
          onChange={enabled => void save(plugin, { enabled, config: plugin.config, expected_version: query.data!.version })} />}>
        <Typography.Paragraph>{plugin.description}</Typography.Paragraph>
        <Space wrap>
          {plugin.hooks.map(hook => <Tag key={hook}>{hook === "content" ? "正文扩展" : "前台资源"}</Tag>)}
          {!plugin.available && <Tag color="warning">插件不可用</Tag>}
          {plugin.available && plugin.config_fields.length > 0 && <Button disabled={busy || query.isError} onClick={() => {
            setError(null); setNotice(null); setDraft({ plugin, config: { ...plugin.config }, version: query.data!.version });
          }}>配置 {plugin.name}</Button>}
        </Space>
      </Card>)}
    </Space>
    <Modal title={draft ? `配置 ${draft.plugin.name}` : "插件配置"} open={draft !== null} destroyOnHidden
      onCancel={() => { if (!busy) { setDraft(null); setError(null); } }}
      cancelButtonProps={{ disabled: busy }} closable={!busy} maskClosable={!busy}
      okText="保存配置" confirmLoading={busy} okButtonProps={{ disabled: !dirty }}
      onOk={() => { if (draft) void save(draft.plugin, { enabled: draft.plugin.enabled, config: draft.config, expected_version: draft.version }); }}>
      {errorNotice && <Alert type="error" showIcon title={errorNotice} style={{ marginBottom: 16 }} />}
      <Form layout="vertical" disabled={busy}>
        {draft?.plugin.config_fields.map(field => <Form.Item key={field.key} label={field.label} help={field.description || undefined}>
          {typeof field.default === "boolean" ? <Switch aria-label={field.label} checked={draft.config[field.key] === true} onChange={value => change(field.key, value)} />
            : typeof field.default === "number" ? <InputNumber aria-label={field.label} min={-2147483648} max={2147483647} precision={0}
              value={draft.config[field.key] as number} onChange={value => { if (value !== null) change(field.key, value); }} />
              : <Input aria-label={field.label} maxLength={2048} value={draft.config[field.key] as string} onChange={event => change(field.key, event.target.value)} />}
        </Form.Item>)}
      </Form>
    </Modal>
  </>;
}
