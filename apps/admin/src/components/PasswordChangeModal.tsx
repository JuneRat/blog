import { Alert, App as AntdApp, Button, Form, Input, Modal, Space, Typography } from "antd";
import { useState } from "react";
import { ApiError, setCsrfToken, withRequestId } from "../api/client";
import { identityApi } from "../api/identity";
import { messageOf as apiMessageOf } from "../apiError";

/**
 * 自助改密弹窗（`POST /api/admin/v1/me/password`）。
 *
 * 三条不变式必须在这里体现，其余交给服务端：
 *
 * 1. **不猜用户有没有密码**：`GET /me` 不回 `password_enabled`，前端无法区分
 *    「已启用密码登录」与「只有外部身份的 OAuth 用户」，所以「当前密码」做成
 *    可留空，由服务端判定是否需要重新认证；留空时请求体里不带该字段。
 * 2. **不复制服务端口令策略**：长度、含用户名、常见口令等由服务端强制并返回
 *    文案；前端只拦「两次新密码不一致」这一纯 UI 一致性问题。
 * 3. **成功必须换内存 token**：服务端撤销该用户全部会话后再签发新会话，响应里的
 *    `csrf_token` 属于新会话；旧 token 立即失效，不写回内存会让后续写请求全部 403。
 *
 * 失败文案留在弹窗内的**内联 Alert**（本仓库约定：需要看清的文案不用 message
 * 吐司），弹窗保持打开，用户可改正后直接重试。
 */
export interface PasswordChangeModalProps {
  open: boolean;
  onClose: () => void;
}

interface PasswordDraft {
  currentPassword: string;
  newPassword: string;
  confirmPassword: string;
}

const EMPTY_DRAFT: PasswordDraft = {
  currentPassword: "",
  newPassword: "",
  confirmPassword: "",
};

/** 失败文案：`code` 分支补充解释，其余错误原样带请求编号展示。 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    switch (error.code) {
      case "invalid_credentials":
        // 403 而不是 401：会话仍然有效，这里必须是「当前密码错」而不是「掉线」，
        // 绝不能触发任何登出/清 token 的动作。
        return withRequestId(
          `当前密码不正确，你仍然处于登录状态，可以改正后重试。（服务端：${error.message}）`,
          error.requestId,
        );
      case "rate_limited":
        // 重新认证与登录共用同一份失败预算，只有服务端知道还剩多久（Retry-After）。
        return withRequestId(
          `${error.message}（当前密码的尝试次数过多，与登录共用失败预算，请稍后再试。）`,
          error.requestId,
        );
      case "version_conflict":
        // 条件写入失败：期间管理员下发了强制重置或别人改了密码，本次改密作废，
        // 且没有覆盖新口令；旧口令已经无效，必须让用户知道原因而不是只看到「版本冲突」。
        return withRequestId(
          `${error.message}（当前口令已失效：期间可能已被管理员重置或由他人修改，本次修改未生效，请重新确认。）`,
          error.requestId,
        );
      default:
        // 其余情况回落到通用文案（含 requestId），不再各写一份 tail。
        return apiMessageOf(error);
    }
  }
  return apiMessageOf(error);
}

export function PasswordChangeModal({ open, onClose }: PasswordChangeModalProps) {
  const { message } = AntdApp.useApp();
  const [form] = Form.useForm<PasswordDraft>();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  /** 关闭即清空：下次打开不残留上一次的密码草稿与错误文案。 */
  function close(): void {
    // 请求飞行期间不允许关闭：结果回来时要写回的 token / 重置的表单，
    // 不能落在一个已经关掉又被重开的弹窗上。
    if (busy) return;
    form.resetFields();
    setError(null);
    onClose();
  }

  async function submit(values: PasswordDraft): Promise<void> {
    const current = values.currentPassword ?? "";
    setError(null);
    setBusy(true);
    try {
      const result = await identityApi.changeOwnPassword({
        // 留空即不下发该字段（JSON.stringify 会丢掉 undefined）：这是 OAuth 用户
        // 设置初始密码的路径，服务端要求此时凭据「必须为空」。
        current_password: current === "" ? undefined : current,
        new_password: values.newPassword,
      });
      // 会话已轮换：旧会话连同旧 CSRF token 一起被撤销，响应里的 token 属于新会话。
      // 必须立刻写回内存 —— 否则之后所有写请求都会 403。
      setCsrfToken(result.csrf_token);
      form.resetFields();
      setError(null);
      // 弹窗即将关闭，成功提示无法留在弹窗内；失败文案才需要停留细读。
      message.success("密码已更新，其他设备上的会话已全部退出。");
      onClose();
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal open={open} title="修改密码" onCancel={close} footer={null} width={480}>
      <Typography.Paragraph type="secondary" style={{ marginTop: 0 }}>
        密码规则（长度、是否包含用户名、常见口令等）由服务端校验，不符合时会在下方给出具体原因。
      </Typography.Paragraph>

      {error !== null && (
        <Alert type="error" showIcon title={error} style={{ marginBottom: 16 }} />
      )}

      <Form
        form={form}
        layout="vertical"
        initialValues={EMPTY_DRAFT}
        onFinish={(values) => void submit(values)}
      >
        <Form.Item
          label="当前密码"
          name="currentPassword"
          extra="已启用密码登录时必须填写；仅用外部身份登录、首次设置密码时留空。"
        >
          <Input.Password autoComplete="current-password" />
        </Form.Item>

        <Form.Item
          label="新密码"
          name="newPassword"
          rules={[{ required: true, message: "请输入新密码" }]}
        >
          <Input.Password autoComplete="new-password" />
        </Form.Item>

        <Form.Item
          label="确认新密码"
          name="confirmPassword"
          dependencies={["newPassword"]}
          rules={[
            { required: true, message: "请再次输入新密码" },
            // 只做「两次输入是否一致」的 UI 校验，不判断口令强度：策略是服务端的边界。
            ({ getFieldValue }) => ({
              validator(_rule, value: string) {
                if (value === undefined || value === "" || value === getFieldValue("newPassword")) {
                  return Promise.resolve();
                }
                return Promise.reject(new Error("两次输入的新密码不一致"));
              },
            }),
          ]}
        >
          <Input.Password autoComplete="new-password" />
        </Form.Item>

        <Space>
          {/*
            用文案切换而不是 Button 的 loading 属性表示进行中：loading 图标的
            `role="img" aria-label="loading"` 会污染按钮的无障碍名（动画结束后
            仍留在 DOM 里），让按名字定位变脆、读屏念出多余的 "loading"。
            与 src/screens/PostEditScreen.tsx 的提交按钮同一约定。
          */}
          <Button type="primary" htmlType="submit" disabled={busy}>
            {busy ? "处理中…" : "更新密码"}
          </Button>
          <Button onClick={close} disabled={busy}>
            取消
          </Button>
        </Space>
      </Form>
    </Modal>
  );
}
