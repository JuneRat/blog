import { Alert, Button, Card, Flex, Form, Input, Typography } from "antd";
import { useRef, useState } from "react";
import { api } from "../api";
import { messageOf } from "../apiError";
import { useAuth } from "../auth";

interface Credentials {
  username: string;
  password: string;
}

/**
 * 登录页：本土密码 + 已绑定的外部身份。
 *
 * 密码登录由后端强制（Argon2id 校验 + 失败限流），前端不缓存任何凭据：
 * 提交后只等 cookie 与 `/me`。失败时清空密码框，避免留在屏幕或表单状态里。
 * 提供商列表来自公开只读 `/auth/providers`（id/展示名/类型）。
 */
export function LoginScreen() {
  const { providers, providersLoaded, refresh } = useAuth();
  const [form] = Form.useForm<Credentials>();
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** 提交中的同步闸门：按钮 disabled 要等下一次渲染，双击仍可能进两次。 */
  const inFlight = useRef(false);
  /**
   * 供按钮可用性判断的镜像值。
   *
   * 不用 `Form.useWatch`：它经 rc-field-form 的 MessageChannel 批处理，
   * 要到下一个宏任务才反映到界面，按钮会比输入慢一拍（测试里同步断言也读不到）。
   * `onValuesChange` 在字段变更的同一个同步流程里回调，这里只镜像不另立数据源。
   */
  const [values, setValues] = useState<Credentials>({ username: "", password: "" });

  const canSubmit =
    values.username.trim().length > 0 && values.password.length > 0 && !submitting;

  /** 口令不在内存里多留一秒：setFieldsValue 不触发 onValuesChange，镜像要同步清。 */
  function clearPassword(): void {
    form.setFieldsValue({ password: "" });
    setValues((prev) => ({ ...prev, password: "" }));
  }

  async function onSubmit(): Promise<void> {
    const current = form.getFieldsValue();
    const name = (current.username ?? "").trim();
    const pass = current.password ?? "";
    if (name.length === 0 || pass.length === 0 || inFlight.current) return;
    inFlight.current = true;
    setSubmitting(true);
    setError(null);
    try {
      await api.loginWithPassword({ username: name, password: pass, next: "/admin/" });
      // 成功后立即从内存状态抹掉口令，再刷新会话。
      clearPassword();
      // 登录成功后重新读取会话与内存 CSRF token，路由守卫随即进入后台。
      await refresh();
    } catch (cause) {
      setError(messageOf(cause));
      clearPassword();
    } finally {
      inFlight.current = false;
      setSubmitting(false);
    }
  }

  return (
    <Flex justify="center" style={{ padding: "12vh 20px" }}>
      <Card style={{ width: "100%", maxWidth: 420 }}>
        <Typography.Title level={3}>博客后台</Typography.Title>
        <Typography.Paragraph type="secondary">
          使用本站账号密码，或已绑定的外部身份登录。
        </Typography.Paragraph>

        <Form
          form={form}
          layout="vertical"
          initialValues={{ username: "", password: "" }}
          onValuesChange={(_changed, all) => setValues(all)}
          onFinish={() => void onSubmit()}
        >
          <Form.Item label="用户名" name="username">
            <Input name="username" autoComplete="username" />
          </Form.Item>
          <Form.Item label="密码" name="password">
            <Input name="password" type="password" autoComplete="current-password" />
          </Form.Item>
          {error !== null && (
            <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />
          )}
          {/* 用文案切换而不是 Button 的 loading：见 PostEditScreen 的同名说明。 */}
          <Button type="primary" htmlType="submit" block disabled={!canSubmit}>
            {submitting ? "正在登录…" : "登录"}
          </Button>
        </Form>

        <Typography.Paragraph type="secondary" style={{ marginTop: 16 }}>
          连续失败会临时锁定账号。登录后可在右上角「修改密码」自助设置或更换密码；
          忘记密码时由运维用 <Typography.Text code>blog user passwd</Typography.Text> 重置。
        </Typography.Paragraph>

        {providersLoaded && providers.length > 0 && (
          <>
            <Typography.Paragraph type="secondary">或使用已绑定的外部身份：</Typography.Paragraph>
            <Flex vertical gap={8}>
              {providers.map((provider) => (
                <Button
                  key={provider.id}
                  href={`/auth/login?provider=${encodeURIComponent(provider.id)}&next=${encodeURIComponent("/admin/")}`}
                >
                  使用 {provider.name} 登录
                </Button>
              ))}
            </Flex>
          </>
        )}
      </Card>
    </Flex>
  );
}
