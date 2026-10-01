import { Input, InputNumber, Select, Switch } from "antd";
import type { ThemeConfigField, ThemeConfigValue } from "../api/generated";
import { CoverPicker } from "./CoverPicker";

/** Scalar controls shared with plugin configuration; constraints come from the caller. */
export function ConfigFieldInput({ field, value, onChange, disabled = false, canReadMedia = false, canUploadMedia = false, scope, enforceMaxLength = false }: {
  field: Pick<ThemeConfigField, "type" | "label" | "min" | "max" | "max_length" | "options">;
  value: ThemeConfigValue; onChange: (value: ThemeConfigValue) => void; disabled?: boolean;
  canReadMedia?: boolean; canUploadMedia?: boolean; scope?: string; enforceMaxLength?: boolean;
}) {
  switch (field.type) {
    case "boolean": return <Switch aria-label={field.label} disabled={disabled} checked={value === true} onChange={onChange} />;
    case "integer": return <InputNumber aria-label={field.label} disabled={disabled} precision={0} min={field.min ?? -Number.MAX_SAFE_INTEGER} max={field.max ?? Number.MAX_SAFE_INTEGER}
      value={typeof value === "number" ? value : null} onChange={value => { if (value !== null) onChange(value); }} />;
    case "select": return <Select aria-label={field.label} disabled={disabled} value={value} options={field.options} onChange={onChange} />;
    case "media": return <CoverPicker label={field.label} value={typeof value === "string" ? value : null} onChange={onChange} disabled={disabled}
      canReadMedia={canReadMedia} canUploadMedia={canUploadMedia} uploadScope={scope} />;
    case "textarea": return <Input.TextArea aria-label={field.label} disabled={disabled} value={typeof value === "string" ? value : ""} onChange={event => onChange(event.target.value)} autoSize={{ minRows: 3, maxRows: 8 }} />;
    case "color": return <Input type="color" aria-label={field.label} disabled={disabled} value={typeof value === "string" ? value : ""} onChange={event => onChange(event.target.value)} style={{ width: 100 }} />;
    case "text": return <Input aria-label={field.label} disabled={disabled} maxLength={enforceMaxLength ? field.max_length ?? undefined : undefined} value={typeof value === "string" ? value : ""} onChange={event => onChange(event.target.value)} />;
  }
}
