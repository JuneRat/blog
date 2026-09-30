import { Button, Input, Space, Typography } from "antd";
import { useEffect, useState } from "react";
import { dateTimeInput, formatDateTime, inputToInstant, invalidLocalTime } from "../timeZone";
import { useTimeZone } from "../timeZoneContext";

export type ContentAction = "publish" | "unpublish" | "schedule" | "archive";

export function statusLabel(status: string): string {
  return ({ draft: "草稿", scheduled: "待发布", published: "已发布", archived: "已归档" })[status] ?? status;
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
  const timeZone = useTimeZone();
  const [at, setAt] = useState(() => dateTimeInput(publishedAt, timeZone));
  useEffect(() => setAt(dateTimeInput(publishedAt, timeZone)), [publishedAt, timeZone]);
  const canSchedule = canPublish && (status === "draft" || status === "scheduled");
  const instant = inputToInstant(at, timeZone);
  const timestamp = instant ? Date.parse(instant) : NaN;
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
          <Input aria-label={`预约发布时间（${timeZone}）`} type="datetime-local" value={at}
            disabled={disabled} onChange={(event) => setAt(event.target.value)} style={{ width: 230 }} />
          <Button disabled={disabled || !Number.isFinite(timestamp) || timestamp <= Date.now()}
            onClick={() => { if (instant && timestamp > Date.now()) void onAction("schedule", instant); }}>
            {status === "scheduled" ? "更新预约" : "预约发布"}
          </Button>
          <Typography.Text type="secondary">站点时区：{timeZone}</Typography.Text>
          {at && !instant && <Typography.Text type="danger" role="alert">{invalidLocalTime}</Typography.Text>}
        </>
      )}
      {status === "scheduled" && publishedAt && (
        <Typography.Text type="secondary">预约：{formatDateTime(publishedAt, timeZone)}</Typography.Text>
      )}
    </Space>
  );
}
