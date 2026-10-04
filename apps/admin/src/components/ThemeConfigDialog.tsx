import { Alert, Button, Form, Modal, Space, Typography } from "antd";
import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { ApiError } from "../api/client";
import type { ThemeConfigField, ThemeConfigSettings, ThemeConfigValue } from "../api/generated";
import { themeSettingsApi } from "../api/settings";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { invalidateAfterWrite } from "../queryEffects";
import { useEditorRequestGuard } from "../useEditorRequestGuard";
import { ConfigFieldInput } from "./ConfigFieldInput";

type Config = Record<string, ThemeConfigValue>;
export function themeFieldError(field: ThemeConfigField, value: ThemeConfigValue): string | null {
  switch (field.type) {
    case "text": case "textarea": {
      if (typeof value !== "string") return "请输入文本";
      const length = Array.from(value).length;
      return length < (field.min_length ?? 0) || length > (field.max_length ?? 8192) ? `长度应在 ${field.min_length ?? 0}–${field.max_length ?? 8192} 个字符之间` : null;
    }
    case "integer": return typeof value !== "number" || !Number.isSafeInteger(value) || value < (field.min ?? -Number.MAX_SAFE_INTEGER) || value > (field.max ?? Number.MAX_SAFE_INTEGER) ? "请输入范围内的整数" : null;
    case "boolean": return typeof value === "boolean" ? null : "请选择布尔值";
    case "select": return field.options.some(o => o.value === value) ? null : "请选择有效选项";
    case "color": return typeof value === "string" && /^#[0-9a-fA-F]{6}$/.test(value) ? null : "请输入六位十六进制颜色";
    case "media": return value === null || (typeof value === "string" && /^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(value) && value !== "00000000-0000-0000-0000-000000000000") ? null : "请选择有效媒体";
  }
}
const same = (a: Config, b: Config) => JSON.stringify(Object.entries(a).sort()) === JSON.stringify(Object.entries(b).sort());
export function ThemeConfigDialog({ slug, name, onClose, onDirty, onSaved }: {
  slug: string; name: string; onClose: () => void; onDirty: (dirty: boolean) => void; onSaved: (saved: ThemeConfigSettings) => void;
}) {
  const { me } = useAuth();
  const client = useQueryClient();
  const [view, setView] = useState<ThemeConfigSettings | null>(null);
  const [config, setConfig] = useState<Config>({});
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);
  const beginRequest = useEditorRequestGuard(slug);
  const dirty = view !== null && !same(config, view.overrides);
  useEffect(() => { onDirty(dirty); return () => onDirty(false); }, [dirty, onDirty]);
  async function load() {
    if (busyRef.current) return;
    const current = beginRequest(); busyRef.current = true; setBusy(true); setError(null);
    try { const next = await themeSettingsApi.getConfig(slug); if (current()) { setView(next); setConfig({ ...next.overrides }); setConflict(false); } }
    catch (cause) { if (current()) setError(permissionMessageOf(cause)); }
    finally { busyRef.current = false; if (current()) setBusy(false); }
  }
  useEffect(() => { void load(); }, [slug]); // Each dialog mounts for one installed theme.
  const fieldErrors = Object.fromEntries((view?.fields ?? []).map(f => [f.key, themeFieldError(f, config[f.key] === undefined ? f.default : config[f.key])]));
  async function save() {
    if (!view || busyRef.current || Object.values(fieldErrors).some(Boolean) || conflict) return;
    const current = beginRequest(); busyRef.current = true; setBusy(true); setError(null);
    try {
      const next = await themeSettingsApi.saveConfig(slug, { id: view.id, expected_release: view.release, config_schema_version: view.config_schema_version, expected_version: view.version, config });
      void invalidateAfterWrite(client, "theme");
      if (current()) { setView(next); setConfig({ ...next.overrides }); onSaved(next); }
    } catch (cause) {
      if (current()) {
        const stale = cause instanceof ApiError && cause.code === "version_conflict";
        setConflict(stale);
        setError(stale ? "主题身份、发布版本或配置已发生变化。当前输入已保留，请重新加载后编辑。" : permissionMessageOf(cause));
      }
    } finally { busyRef.current = false; if (current()) setBusy(false); }
  }
  const groups = new Map<string, ThemeConfigField[]>();
  for (const field of view?.fields ?? []) groups.set(field.group, [...(groups.get(field.group) ?? []), field]);
  return <Modal title={`配置主题「${name}」`} open destroyOnHidden onCancel={() => { if (!busy) onClose(); }} keyboard={!busy} closable={!busy} mask={{ closable: !busy && !dirty }}
    cancelButtonProps={{ disabled: busy }} okText="保存配置" confirmLoading={busy}
    okButtonProps={{ "aria-label": "保存配置", disabled: busy || !dirty || conflict || Object.values(fieldErrors).some(Boolean) }} onOk={() => void save()}>
    {error && <Alert type="error" showIcon title={error} action={<Button disabled={busy} onClick={() => void load()}>重新加载配置</Button>} style={{ marginBottom: 16 }} />}
    {!view && busy && <Typography.Paragraph role="status">正在加载主题配置…</Typography.Paragraph>}
    {view && <>
      <Typography.Paragraph type="secondary">配置独立保存，激活此主题后生效。发布 {view.release.slice(0, 12)} · 配置 v{view.version}</Typography.Paragraph>
      {view.fields.length === 0 && <Typography.Paragraph>此主题没有可配置字段。</Typography.Paragraph>}
      <Form layout="vertical" disabled={busy}>
        {[...groups].map(([group, fields]) => <div key={group}>
          {group && <Typography.Title level={5}>{group}</Typography.Title>}
          {fields.map(field => <Form.Item key={field.key} label={field.label} validateStatus={fieldErrors[field.key] ? "error" : undefined} help={fieldErrors[field.key] ?? (field.description || undefined)}>
            <ConfigFieldInput field={field} value={config[field.key] === undefined ? field.default : config[field.key]} disabled={busy} scope={`${slug}:${view.id}:${field.key}`}
              canReadMedia={me?.permissions.includes("media.read")} canUploadMedia={me?.permissions.includes("media.upload")}
              onChange={value => setConfig(draft => ({ ...draft, [field.key]: value }))} />
            <Space style={{ marginTop: 4 }}><Button size="small" type="link" disabled={busy || !Object.hasOwn(config, field.key)} aria-label={`恢复默认 ${field.label}`}
              onClick={() => setConfig(draft => { const next = { ...draft }; delete next[field.key]; return next; })}>恢复默认值</Button></Space>
          </Form.Item>)}
        </div>)}
      </Form>
    </>}
  </Modal>;
}
