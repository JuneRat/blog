import { Alert, Button, Form, Input, Space, Spin, Typography } from "antd";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { ApiError, api } from "../api";
import { messageOf } from "../apiError";
import { useAuth } from "../auth";
import { queryKeys } from "../queryClient";
import type { Profile } from "../types";
import { useUnsavedGuard } from "../unsaved";

export function ProfileScreen() {
  const { me } = useAuth();
  const profile = useQuery({
    queryKey: ["own-profile", me?.user_id],
    queryFn: () => api.me(),
    staleTime: 0,
    gcTime: 0,
  });
  return (
    <>
      <Typography.Title level={3}>个人资料</Typography.Title>
      {profile.isPending ? <Spin /> : profile.data ? (
        <ProfileForm key={profile.data.user_id} initial={profile.data} />
      ) : (
        <Alert type="error" showIcon title={messageOf(profile.error)}
          action={<Button onClick={() => void profile.refetch()}>重试</Button>} />
      )}
    </>
  );
}

function ProfileForm({ initial }: { initial: Profile }) {
  const { refresh } = useAuth();
  const queryClient = useQueryClient();
  const [saved, setSaved] = useState(initial);
  const [displayName, setDisplayName] = useState(initial.display_name ?? "");
  const [bio, setBio] = useState(initial.bio ?? "");
  const [busy, setBusy] = useState(false);
  const [conflict, setConflict] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const dirty = displayName !== (saved.display_name ?? "") || bio !== (saved.bio ?? "");
  useUnsavedGuard(dirty, "个人资料尚未保存，离开会丢失修改。");

  function accept(profile: Profile) {
    setSaved(profile);
    setDisplayName(profile.display_name ?? "");
    setBio(profile.bio ?? "");
    setConflict(false);
  }

  async function save() {
    if (busy || conflict || !dirty) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const profile = await api.updateOwnProfile({
        display_name: displayName.trim() || null,
        bio: bio === "" ? null : bio,
        expected_version: saved.version,
      });
      // 使用本次提交返回的版本；刷新其他展示失败也不把已保存的输入当作未保存。
      accept(profile);
      setNotice("个人资料已保存。");
      await queryClient.invalidateQueries({ queryKey: queryKeys.users() });
      try {
        await refresh();
      } catch (e) {
        setError(`资料已保存，账号信息刷新失败：${messageOf(e)}`);
      }
    } catch (e) {
      if (e instanceof ApiError && e.code === "version_conflict") {
        setConflict(true);
        setError("资料已在其他位置更新，你的输入已保留。请重新加载最新资料后再修改。");
      } else {
        setError(messageOf(e));
      }
    } finally {
      setBusy(false);
    }
  }

  async function reload() {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      accept(await api.me());
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Form layout="vertical" onFinish={() => void save()} style={{ maxWidth: 560 }}>
      <Form.Item label="用户名"><Typography.Text>{saved.username}</Typography.Text></Form.Item>
      <Form.Item label="展示名" htmlFor="profile-name" extra="留空时使用用户名。">
        <Input id="profile-name" value={displayName} maxLength={100} disabled={busy}
          onChange={(e) => setDisplayName(e.target.value)} />
      </Form.Item>
      <Form.Item label="个人简介" htmlFor="profile-bio" extra="简介按纯文本保存。">
        <Input.TextArea id="profile-bio" value={bio} rows={5} disabled={busy}
          onChange={(e) => setBio(e.target.value)} />
      </Form.Item>
      {error && <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />}
      {notice && <Alert type="success" showIcon title={notice} style={{ marginBottom: 16 }} />}
      <Space>
        <Button type="primary" htmlType="submit" loading={busy} disabled={!dirty || conflict || busy}>
          保存资料
        </Button>
        {conflict && <Button disabled={busy} onClick={() => void reload()}>重新加载并放弃修改</Button>}
      </Space>
    </Form>
  );
}
