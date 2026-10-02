import { Alert, Button, Card, Flex, Form, Input, Typography } from "antd";
import { useRef, useState } from "react";
import { identityApi } from "../api/identity";
import { messageOf } from "../apiError";

export function PasswordRecoveryScreen({ token, onBack }: { token?: string; onBack: () => void }) {
  const [form] = Form.useForm<{ email: string; password: string; confirm: string }>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [complete, setComplete] = useState(false);
  const inFlight = useRef(false);
  const resetting = token !== undefined;
  async function submit() {
    if (inFlight.current || complete) return;
    const input = form.getFieldsValue();
    if (resetting && input.password !== input.confirm) { setError("两次输入的密码不一致。"); return; }
    inFlight.current = true;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const result = resetting
        ? await identityApi.resetPassword({ token, password: input.password })
        : await identityApi.requestPasswordRecovery({ email: input.email.trim() });
      setNotice(result.message);
      setComplete(resetting);
    } catch (cause) { setError(messageOf(cause)); }
    finally {
      form.setFieldsValue({ password: "", confirm: "" });
      inFlight.current = false;
      setBusy(false);
    }
  }
  return <Flex justify="center" style={{ padding: "12vh 20px" }}>
    <Card style={{ width: "100%", maxWidth: 460 }}>
      <Typography.Title level={3}>{resetting ? "设置登录密码" : "找回密码"}</Typography.Title>
      <Typography.Paragraph type="secondary">{resetting
        ? "邀请或重置链接在 30 分钟内有效，只能使用一次。密码设置成功后，需要重新登录。"
        : "输入账号邮箱，我们会发送一次性密码重置链接。"}</Typography.Paragraph>
      {error && <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />}
      {notice && <Alert type="success" showIcon title={notice} style={{ marginBottom: 16 }} />}
      {!complete && <Form form={form} layout="vertical" onFinish={() => void submit()} disabled={busy}>
        {resetting ? <>
          <Form.Item name="password" label="新密码" rules={[{ required: true, message: "请输入新密码" }]} extra="12–128 个字符，不包含用户名，请避免常见或重复口令。">
            <Input.Password autoComplete="new-password" />
          </Form.Item>
          <Form.Item name="confirm" label="再次输入新密码" rules={[{ required: true, message: "请再次输入新密码" }]}>
            <Input.Password autoComplete="new-password" />
          </Form.Item>
        </> : <Form.Item name="email" label="账号邮箱" rules={[{ required: true, type: "email", message: "请输入有效邮箱" }]}>
          <Input autoComplete="email" maxLength={254} />
        </Form.Item>}
        <Button htmlType="submit" type="primary" block disabled={busy}>{busy ? "正在处理…" : resetting ? "设置密码" : "发送重置邮件"}</Button>
      </Form>}
      <Button style={{ marginTop: 16 }} onClick={onBack} disabled={busy}>返回登录</Button>
    </Card>
  </Flex>;
}
