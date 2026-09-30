import { Tabs, Typography } from "antd";
import { useState } from "react";
import { useUnsavedGuard } from "../unsaved";
import { AccessSettingsForm } from "../components/AccessSettingsForm";
import { RetentionSettingsForm } from "../components/RetentionSettingsForm";
import { SiteSettingsForm } from "./settings/SiteSettingsForm";
import { ThemeSettingsForm } from "./settings/ThemeSettingsForm";
import { HtmlRebuildPanel } from "./settings/HtmlRebuildPanel";

/** Each group owns its form; navigation reads their combined unsaved state. */
export function SettingsScreen() {
  const [activeTab, setActiveTab] = useState("site");
  const [siteDirty, setSiteDirty] = useState(false);
  const [themeDirty, setThemeDirty] = useState(false);
  const [retentionDirty, setRetentionDirty] = useState(false);
  useUnsavedGuard(siteDirty || themeDirty || retentionDirty, "站点设置有未保存的修改，离开会丢失。");
  return <>
    <Typography.Title level={3} style={{ marginBottom: 20 }}>站点设置</Typography.Title>
    <Tabs activeKey={activeTab} onChange={setActiveTab} items={[
      { key: "site", label: "常规设置", forceRender: true, children: <SiteSettingsForm onDirtyChange={setSiteDirty} /> },
      { key: "theme", label: "主题外观", forceRender: true, children: <div style={{ paddingTop: 8 }}><ThemeSettingsForm onDirtyChange={setThemeDirty} /></div> },
      { key: "access", label: "账号与评论", children: <AccessSettingsForm /> },
      { key: "retention", label: "数据保留", forceRender: true, children: <div style={{ paddingTop: 8 }}><RetentionSettingsForm onDirtyChange={setRetentionDirty} /></div> },
      { key: "maintenance", label: "内容维护", children: <HtmlRebuildPanel active={activeTab === "maintenance"} /> },
    ]} />
  </>;
}
