import { Button, Input, Space, Typography } from "antd";
import { useEffect, useState } from "react";

export type ContentAction = "publish" | "unpublish" | "schedule" | "archive";

export function statusLabel(status: string): string {
  return ({ draft: "草稿", scheduled: "待发布", published: "已发布", archived: "已归档" })[status] ?? status;
}

function localTime(value: string | null): string {
  if (!value) return "";
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return "";
  return new Date(date.getTime() - date.getTimezoneOffset() * 60_000).toISOString().slice(0, 16);
}

export function ContentLifecycleControls({ status, publishedAt, disabled, canPublish, canUnpublish, canArchive, onAction }: {
  status: string;
  publishedAt: string | null;
  disabled: boolean;
  canPublish: boolean;
  canUnpublish: boolean;
  canArchive: boolean;
  onAction: (action: ContentAction, at?: string) => Promise<void>;
}) {
  const [at, setAt] = useState(() => localTime(publishedAt));
  useEffect(() => setAt(localTime(publishedAt)), [publishedAt]);
  const canSchedule = canPublish && (status === "draft" || status === "scheduled");
  const timestamp = new Date(at).getTime();
  return (
    <Space wrap>
      {canPublish && status !== "published" && status !== "archived" && (
        <Button disabled={disabled} onClick={() => void onAction("publish")}>发布</Button>
      )}
      {canUnpublish && status !== "draft" && (
        <Button disabled={disabled} onClick={() => void onAction("unpublish")}>
          {status === "scheduled" ? "取消预约" : status === "archived" ? "退回草稿" : "撤回为草稿"}
        </Button>
      )}
      {canArchive && status !== "archived" && (
        <Button disabled={disabled} onClick={() => void onAction("archive")}>归档</Button>
      )}
      {canSchedule && (
        <>
          <Input aria-label="预约发布时间（本地时间）" type="datetime-local" value={at}
            disabled={disabled} onChange={(event) => setAt(event.target.value)} style={{ width: 230 }} />
          <Button disabled={disabled || !Number.isFinite(timestamp) || timestamp <= Date.now()}
            onClick={() => void onAction("schedule", new Date(at).toISOString())}>
            {status === "scheduled" ? "更新预约" : "预约发布"}
          </Button>
          <Typography.Text type="secondary">使用本地时间</Typography.Text>
        </>
      )}
      {status === "scheduled" && publishedAt && (
        <Typography.Text type="secondary">预约：{new Date(publishedAt).toLocaleString()}</Typography.Text>
      )}
    </Space>
  );
}
