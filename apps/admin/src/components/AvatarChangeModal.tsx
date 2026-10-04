import { invalidateAfterWrite } from "../queryEffects";
import { Alert, Modal } from "antd";
import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { identityApi } from "../api/identity";
import { ApiError, withRequestId } from "../api/client";
import { permissionMessageOf } from "../apiError";
import { useAuth } from "../auth";
import { CoverPicker } from "./CoverPicker";

/** Loaded only when the account's avatar dialog is opened. */
export function AvatarChangeModal({ onClose }: { onClose: () => void }) {
  const queryClient = useQueryClient();
  const { me, refresh } = useAuth();
  /** 头像弹窗：选择值单独存一份，点「保存」前不影响头部显示。 */
  const [avatarValue, setAvatarValue] = useState<string | null>(me?.avatar_media_id ?? null);
  const [avatarVersion, setAvatarVersion] = useState<number | null>(me?.version ?? null);
  const [avatarBusy, setAvatarBusy] = useState(false);
  const [avatarError, setAvatarError] = useState<string | null>(null);
  const canReadMedia = me?.permissions.includes("media.read") ?? false;
  const canUploadMedia = me?.permissions.includes("media.upload") ?? false;

  /** 保存头像：本人自助接口；成功后刷新 `/me` 让头部立即更新。 */
  async function saveAvatar(): Promise<void> {
    if (avatarVersion === null || avatarBusy) return;
    setAvatarBusy(true);
    setAvatarError(null);
    try {
      await identityApi.setOwnAvatar(avatarValue, avatarVersion);
      void invalidateAfterWrite(queryClient, "profile");
      await refresh?.();
      onClose();
    } catch (e) {
      if (e instanceof ApiError && e.code === "version_conflict") {
        setAvatarVersion(null);
        setAvatarError(withRequestId("个人资料已在其他窗口更新，请关闭窗口后重新选择头像。", e.requestId));
        // 保留当前选择供查看；刷新成功后也必须重新打开弹窗确认，不能静默重试覆盖。
        try { await refresh?.(); } catch { /* 下次打开仍使用旧版本，服务端继续拒绝覆盖。 */ }
      } else {
        setAvatarError(permissionMessageOf(e));
      }
    } finally {
      setAvatarBusy(false);
    }
  }

  return (
    <Modal
      title="更换头像"
      open
      okText="保存"
      cancelText="取消"
      confirmLoading={avatarBusy}
      okButtonProps={{ disabled: avatarVersion === null }}
      onOk={() => void saveAvatar()}
      onCancel={onClose}
      destroyOnHidden
    >
      <CoverPicker
        value={avatarValue}
        onChange={setAvatarValue}
        // 服务端下发的地址只对「当前头像 id」有效：一旦在弹窗里换了图，
        // 必须回退到 mediaUrl(value) 显示新图，否则预览停在旧头像上。
        currentUrl={
          avatarValue === (me?.avatar_media_id ?? null) ? (me?.avatar_url ?? null) : null
        }
        canReadMedia={canReadMedia}
        canUploadMedia={canUploadMedia}
        disabled={avatarBusy}
        label="头像"
      />
      {avatarError !== null && (
        <Alert type="error" showIcon title={avatarError} style={{ marginTop: 12 }} />
      )}
    </Modal>
  );
}
