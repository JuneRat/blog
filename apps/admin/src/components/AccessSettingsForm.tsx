import { Alert, Divider, Flex, Switch, Typography } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { identityApi } from "../api/identity";
import { CommentSwitch } from "./CommentSwitch";
import { messageOf } from "../apiError";

const queryKey = ["accessSettings"];
export function AccessSettingsForm() {
  const client = useQueryClient();
  const policy = useQuery({ queryKey, queryFn: identityApi.accessSettings });
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  async function change(key: "registration_enabled" | "guest_comments_enabled", value: boolean) {
    if (!policy.data || inFlight.current) return;
    inFlight.current = true; setBusy(true); setError(null); setSaved(false);
    try {
      const updated = await identityApi.saveAccessSettings({ ...policy.data, [key]: value });
      client.setQueryData(queryKey, updated); setSaved(true);
    } catch (cause) {
      setError(messageOf(cause));
      void policy.refetch();
    } finally { inFlight.current = false; setBusy(false); }
  }
  return <section aria-label="账号与评论设置">
    <Typography.Title level={4}>账号与评论</Typography.Title>
    <Typography.Paragraph type="secondary">开关修改后立即保存。新注册账号为 reader，只能发表评论和管理个人资料。</Typography.Paragraph>
    {(error || policy.error) && <Alert type="error" title={error || messageOf(policy.error)} showIcon />}
    {saved && <Alert type="success" title="已保存" showIcon />}
    <Flex vertical gap={24} style={{ marginTop: 24 }}>
      <Flex gap={12} align="center"><Switch aria-label="开放注册" checked={policy.data?.registration_enabled ?? false} disabled={!policy.data || busy} onChange={value => void change("registration_enabled", value)} /><span>开放注册</span></Flex>
    </Flex>
    <Divider />
    <CommentSwitch />
    <Flex vertical gap={24} style={{ marginTop: 24 }}>
      <Flex gap={12} align="center"><Switch aria-label="开放游客评论" checked={policy.data?.guest_comments_enabled ?? false} disabled={!policy.data || busy} onChange={value => void change("guest_comments_enabled", value)} /><span>开放游客评论</span></Flex>
    </Flex>
    <Typography.Paragraph type="secondary" style={{ marginTop: 24 }}>游客评论关闭时需要登录。发表评论须同时满足全站及文章评论开关，审核策略也适用于回复。</Typography.Paragraph>
  </section>;
}
