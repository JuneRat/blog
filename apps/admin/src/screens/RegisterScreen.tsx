import { Alert, Button, Card, Flex, Form, Input, Typography } from "antd";
import { useRef, useState } from "react";
import { api } from "../api";
import type { RegistrationInput } from "../api/generated";
import { messageOf } from "../apiError";

export function RegisterScreen({ onBack, onRegistered }: {
  onBack: () => void;
  onRegistered: (username: string) => void;
}) {
  const [form] = Form.useForm<RegistrationInput>();
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(input: RegistrationInput) {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      await api.register({
        ...input,
        username: input.username.trim(),
        email: input.email.trim(),
        display_name: input.display_name?.trim() || null,
      });
      form.resetFields();
      onRegistered(input.username.trim());
    } catch (cause) {
      setError(messageOf(cause));
      form.setFieldValue("password", "");
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  }

  return (
    <Flex justify="center" style={{ padding: "8vh 20px" }}>
      <Card style={{ width: "100%", maxWidth: 420 }}>
        <Typography.Title level={3}>注册账号</Typography.Title>
        <Typography.Paragraph type="secondary">
          注册后可发表评论、管理个人资料。
        </Typography.Paragraph>
        <Form form={form} layout="vertical" onFinish={(input) => void submit(input)} disabled={busy}>
          <Form.Item label="用户名" name="username" rules={[
            { required: true, whitespace: true, message: "请填写用户名" },
            { pattern: /^[A-Za-z0-9_-]{1,64}$/, message: "使用 1–64 个字母、数字、短横线或下划线" },
          ]}>
            <Input autoComplete="username" />
          </Form.Item>
          <Form.Item label="昵称（可选）" name="display_name">
            <Input autoComplete="nickname" />
          </Form.Item>
          <Form.Item label="邮箱" name="email" rules={[
            { required: true, message: "请填写邮箱" },
            { type: "email", message: "请填写有效邮箱" },
          ]}>
            <Input type="email" autoComplete="email" />
          </Form.Item>
          <Form.Item label="密码" name="password" rules={[{ required: true, message: "请填写密码" }]}>
            <Input.Password autoComplete="new-password" />
          </Form.Item>
          <Typography.Paragraph type="secondary">
            密码为 12–128 个字符，不能包含用户名或使用常见弱密码。
          </Typography.Paragraph>
          {error && <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />}
          <Button type="primary" htmlType="submit" block disabled={busy}>
            {busy ? "正在注册…" : "注册"}
          </Button>
        </Form>
        <Button type="link" onClick={onBack} disabled={busy}>返回登录</Button>
      </Card>
    </Flex>
  );
}
