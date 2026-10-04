import { Alert, App, Button, Empty, Modal, Space, Tag, Typography } from "antd";
import { useCallback, useEffect, useRef, useState } from "react";
import { draftSize, listStoredDrafts, removeStoredDraft, type StoredDraft } from "../draftManagement";
import { navigate, paths } from "../router";
import { useLeaveConfirmation } from "../unsaved";
import { formatDateTime } from "../timeZone";
import { useTimeZone } from "../timeZoneContext";

const deletionMessage = {
  active: "该副本正在被编辑器使用，不能删除。请关闭对应编辑器后刷新。",
  changed: "副本在确认期间已有更新，未删除。请重新查看内容。",
  unknown: "无法确认该副本是否仍被其它窗口使用，已保留。请使用支持安全清理的浏览器。",
  missing: "该副本已不存在，列表已刷新。",
};

export default function LocalDraftManager({ owner, kind, id, disabled, onClose, onRestore, onDeleted }: {
  owner: string; kind: "post" | "page"; id: string | null; disabled: boolean;
  onClose: () => void; onRestore: (draft: StoredDraft) => string | null; onDeleted: (key: string) => void;
}) {
  const { modal } = App.useApp();
  const leave = useLeaveConfirmation();
  const timeZone = useTimeZone();
  const [drafts, setDrafts] = useState<StoredDraft[]>([]);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [removing, setRemoving] = useState(false);
  const generation = useRef(0);
  const refresh = useCallback(async () => {
    const visit = ++generation.current;
    setLoading(true);
    try {
      const next = await listStoredDrafts(owner);
      if (visit !== generation.current) return;
      setDrafts(next);
      setSelectedKey(current => next.some(draft => draft.key === current) ? current : next[0]?.key ?? null);
    } catch {
      if (visit === generation.current) setError("无法读取本机副本，浏览器可能未允许访问站点存储。");
    } finally {
      if (visit === generation.current) setLoading(false);
    }
  }, [owner]);
  useEffect(() => {
    void refresh();
    const onStorage = (event: StorageEvent) => {
      if (event.key === null || event.key.startsWith(`blog:local-draft:v2:${encodeURIComponent(owner)}:`) ||
        event.key.startsWith(`blog:local-draft:v1:${encodeURIComponent(owner)}:`)) void refresh();
    };
    window.addEventListener("storage", onStorage);
    return () => { generation.current += 1; window.removeEventListener("storage", onStorage); };
  }, [owner, refresh]);

  const selected = drafts.find(draft => draft.key === selectedKey);
  const sameEditor = selected?.kind === kind && selected.id === id;
  const total = drafts.reduce((bytes, draft) => bytes + draft.bytes, 0);
  function remove(draft: StoredDraft) {
    modal.confirm({ title: "删除本机恢复副本？", content: `将永久删除「${draft.title}」的这份本机副本。服务器内容不受影响。`,
      okText: "删除副本", cancelText: "保留副本", okButtonProps: { danger: true },
      onOk: async () => {
        setRemoving(true);
        try {
          const result = await removeStoredDraft(draft);
          if (result === "deleted") { onDeleted(draft.key); setError(null); }
          else setError(deletionMessage[result]);
          await refresh();
        } finally { setRemoving(false); }
      },
    });
  }
  function restore(draft: StoredDraft) {
    modal.confirm({ title: "恢复到当前编辑器？", content: "恢复会替换当前编辑器输入，并保留源副本。请先保存或复制需要保留的内容。恢复不会提交到服务器。",
      okText: "恢复副本", cancelText: "取消", onOk: () => {
        const problem = onRestore(draft);
        if (problem) { setError(problem); void refresh(); }
        else onClose();
      },
    });
  }
  function openEditor(draft: StoredDraft) {
    leave(() => {
      onClose();
      navigate(draft.kind === "post" ? draft.id === null ? paths.newPost : paths.editPost(draft.id) :
        draft.id === null ? paths.newPage : paths.editPage(draft.id));
    });
  }

  return <Modal open title="管理本机恢复副本" onCancel={onClose} footer={null} width={760}>
    <Space orientation="vertical" style={{ width: "100%" }}>
      <Typography.Text>当前账号共 {drafts.length} 份副本，约占 {draftSize(total)}（按存储文本估算）。仅清理本浏览器内的恢复副本。</Typography.Text>
      <Typography.Text type="secondary">副本代表本机输入；服务器是否已有同样内容，请恢复后核对。正在使用的副本受保护。浏览器无法确认安全时，仍可查看和恢复。</Typography.Text>
      {error && <Alert type="warning" showIcon title={error} />}
      <Button size="small" loading={loading} disabled={removing} onClick={() => { setError(null); void refresh(); }}>刷新副本列表</Button>
      {!loading && drafts.length === 0 && <Empty description="当前账号没有本机恢复副本" />}
      <div style={{ maxHeight: 280, overflow: "auto", width: "100%" }}>
        {drafts.map(draft => <div key={draft.key} style={{ padding: 12, borderBottom: "1px solid var(--ant-color-border-secondary)" }}>
          <Space orientation="vertical" size={4} style={{ width: "100%" }}>
            <Typography.Text strong>{draft.title}</Typography.Text>
            <Typography.Text type="secondary">{draft.kind === "post" ? "文章" : "页面"} · {draft.savedAt ? formatDateTime(draft.savedAt, timeZone) : "时间未知"} · {draftSize(draft.bytes)}</Typography.Text>
            <Space wrap size={4}>
              <Tag>{!draft.value ? "副本格式不可恢复" : draft.baselineVersion === null ? "尚未创建服务器内容" : `基于服务器 v${draft.baselineVersion} 的本机编辑`}</Tag>
              <Tag color={draft.activity === "active" ? "orange" : undefined}>{draft.activity === "active" ? "编辑器正在使用" : draft.activity === "idle" ? "可安全清理" : "使用状态无法确认"}</Tag>
              {draft.writerId === null && <Tag>旧版源副本</Tag>}
            </Space>
            <Space>
              <Button size="small" aria-label={`查看副本：${draft.title}`} onClick={() => setSelectedKey(draft.key)}>查看</Button>
              <Button size="small" danger disabled={disabled || removing || draft.activity !== "idle"} aria-label={`删除副本：${draft.title}`} onClick={() => remove(draft)}>删除</Button>
            </Space>
          </Space>
        </div>)}
      </div>
      {selected && <div style={{ width: "100%" }}>
        <Typography.Title level={5}>预览：{selected.title}</Typography.Title>
        <pre aria-label="副本正文预览" style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere", maxHeight: 220, overflow: "auto" }}>
          {typeof selected.value?.content === "string" ? selected.value.content : selected.raw}
        </pre>
        {selected.value && <Button disabled={disabled || removing || loading} onClick={() => sameEditor ? restore(selected) : openEditor(selected)}>
          {sameEditor ? "恢复到当前编辑器" : "打开对应编辑器恢复"}
        </Button>}
      </div>}
    </Space>
  </Modal>;
}
