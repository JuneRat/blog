import { Alert, Button, Select, Space, Typography } from "antd";
import { useEffect, useRef, useState } from "react";
import { candidateRevision, draftIdentity, draftKey, draftScope, handledKey, readCandidates, readHandled, snapshot, type DraftCandidate, type DraftIdentity } from "./draftStorage";

interface DraftState<T> {
  visit: number; key: string; candidates: DraftCandidate<T>[]; selected: string | null;
  reviewing: boolean; savedAt: string | null; error: string | null;
}
const originLabel = { current: "当前窗口", tab: "本标签先前或复制窗口", other: "其它标签", legacy: "旧版恢复副本" };

export function useLocalDraft<T>({ owner, kind, id, ready, disabled, dirty, value, template, baselineVersion, onRestore, identity = draftIdentity() }: {
  owner: string | undefined; kind: "post" | "page"; id: string | null; ready: boolean; disabled: boolean; dirty: boolean;
  value: T; template: T; baselineVersion: number | null; onRestore: (value: T, version: number | null) => void;
  identity?: DraftIdentity;
}) {
  const storageScope = owner ? draftScope(owner, kind, id) : "";
  const key = owner ? draftKey(storageScope, identity) : "";
  const [state, setState] = useState<DraftState<T>>({ visit: -1, key: "", candidates: [], selected: null, reviewing: false, savedAt: null, error: null });
  const activeKey = useRef("");
  const deletedValue = useRef<string | null>(null);
  const adoptedKey = useRef<string | null>(null);
  const handled = useRef<Record<string, string>>({});
  const restoredSource = useRef<DraftCandidate<T> | null>(null);
  const scope = useRef({ key, visit: 0 });
  if (scope.current.key !== key) {
    scope.current = { key, visit: scope.current.visit + 1 };
    activeKey.current = "";
    restoredSource.current = null;
  }
  const visit = scope.current.visit;
  const serialized = JSON.stringify(value);

  function discover() {
    const stored = readCandidates(storageScope, identity, template, id === null);
    return stored.filter(candidate => handled.current[candidate.key] !== candidateRevision(candidate));
  }
  useEffect(() => {
    if (!key || !ready || activeKey.current === key) return;
    activeKey.current = key;
    deletedValue.current = null;
    handled.current = readHandled(storageScope, identity);
    try {
      let candidates = discover();
      const own = candidates.find(candidate => candidate.key === key);
      if (adoptedKey.current === key && dirty && own && JSON.stringify(own.snapshot.value) === serialized) {
        candidates = candidates.filter(candidate => candidate.key !== key);
      }
      adoptedKey.current = null;
      setState({ visit, key, candidates, selected: candidates[0]?.key ?? null, reviewing: candidates.length > 0, savedAt: own?.snapshot.savedAt ?? null, error: null });
    } catch {
      setState({ visit, key, candidates: [], selected: null, reviewing: false, savedAt: null, error: "无法读取本机恢复副本；当前输入尚未保存到本机，请手动保存或复制正文。" });
    }
  }, [key, visit, ready, template, dirty, serialized]);

  useEffect(() => {
    if (!key || !ready || state.key !== key || state.visit !== visit || (state.reviewing && state.candidates.some(candidate => candidate.key === key)) || state.error) return;
    if (deletedValue.current === serialized) return;
    try {
      if (!dirty) {
        localStorage.removeItem(key);
        if (state.savedAt !== null) setState(current => ({ ...current, savedAt: null }));
      } else {
        const draft = snapshot(value, baselineVersion);
        localStorage.setItem(key, JSON.stringify(draft));
        setState(current => ({ ...current, savedAt: draft.savedAt }));
      }
    } catch {
      setState(current => ({ ...current, error: "本机恢复副本保存失败，可能是浏览器存储已满或不可用。请手动保存或复制正文。" }));
    }
    // Timestamps are feedback, not new input to persist.
  }, [key, visit, ready, state.key, state.visit, state.reviewing, state.candidates, state.error, dirty, serialized, baselineVersion]);

  function markHandled(candidate: DraftCandidate<T>) {
    handled.current[candidate.key] = candidateRevision(candidate);
    try { sessionStorage.setItem(handledKey(storageScope, identity), JSON.stringify(handled.current)); }
    catch { /* Never delete another window's source slot to remember this window's choice. */ }
  }
  function closeCandidate(candidate: DraftCandidate<T>, restored: boolean) {
    if (!restored) markHandled(candidate);
    setState(current => {
      const candidates = current.candidates.filter(item => item.key !== candidate.key);
      return { ...current, candidates, selected: candidates[0]?.key ?? null, reviewing: !restored && candidates.length > 0 };
    });
  }
  function discardCurrent() {
    if (!key || disabled) return;
    try {
      // This document only owns this exact key; other windows and legacy slots are never removed.
      localStorage.removeItem(key);
      deletedValue.current = serialized;
      setState(current => ({ ...current, savedAt: null, error: null }));
    } catch {
      setState(current => ({ ...current, error: "浏览器未允许删除本机副本，请在浏览器站点存储设置中清除。" }));
    }
  }
  function refreshCandidates() {
    try {
      const candidates = discover().filter(candidate => candidate.key !== key);
      setState(current => ({ ...current, candidates, selected: candidates[0]?.key ?? null, reviewing: candidates.length > 0 }));
    } catch { setState(current => ({ ...current, error: "读取其它恢复副本失败，当前输入仍保留。" })); }
  }

  /** Commit feedback may remove or migrate only this document's own slot. */
  function saved(nextId: string, nextVersion: number, merged: T, stillDirty: boolean) {
    if (!owner || !key) return;
    try {
      localStorage.removeItem(key);
      const nextKey = draftKey(draftScope(owner, kind, nextId), identity);
      adoptedKey.current = nextKey !== key ? nextKey : null;
      deletedValue.current = null;
      let savedAt: string | null = null;
      if (stillDirty) {
        const draft = snapshot(merged, nextVersion);
        localStorage.setItem(nextKey, JSON.stringify(draft));
        savedAt = draft.savedAt;
      } else localStorage.removeItem(nextKey);
      if (restoredSource.current) { markHandled(restoredSource.current); restoredSource.current = null; }
      setState(current => ({ ...current, candidates: current.candidates.filter(candidate => candidate.key !== key), reviewing: false, error: null, savedAt }));
    } catch {
      setState(current => ({ ...current, reviewing: false, error: "内容已保存，但当前窗口的本机副本未能更新或清除。下次恢复前请核对时间与版本。" }));
    }
  }

  const current = state.key === key && state.visit === visit ? state : null;
  const selected = current?.candidates.find(candidate => candidate.key === current.selected) ?? null;
  const blocksEditing = current?.reviewing === true && current.candidates.some(candidate => candidate.key === key);
  return {
    saved,
    blocksEditing,
    panel: !key || !ready ? null : <div style={{ marginBottom: 16 }}>
      {current?.error && <Alert type="warning" showIcon title={current.error} action={<Button disabled={disabled} onClick={discardCurrent}>删除本机副本</Button>} />}
      {current?.reviewing && selected ? <Alert type="info" showIcon title="发现本机未保存的编辑" description={<Space orientation="vertical" style={{ width: "100%" }}>
        <Select aria-label="选择本机恢复副本" disabled={disabled} value={selected.key} style={{ width: "100%", minWidth: 0 }}
          options={current.candidates.map(candidate => ({ value: candidate.key, label: `${originLabel[candidate.origin]} · ${new Date(candidate.snapshot.savedAt).toLocaleString()} · ${candidate.snapshot.baselineVersion === null ? "新草稿" : `v${candidate.snapshot.baselineVersion}`}` }))}
          onChange={selected => setState(previous => ({ ...previous, selected }))} />
        {blocksEditing && <Typography.Text>请先恢复或丢弃当前窗口已有副本，再开始编辑。</Typography.Text>}
        <Typography.Text>找到 {current.candidates.length} 份副本。恢复只填入编辑器，不会发送到服务器。其它窗口的源副本会保留。</Typography.Text>
        {selected.snapshot.baselineVersion !== baselineVersion && <Typography.Text type="warning">服务器版本已变化，恢复后请先对比再保存。</Typography.Text>}
        <Space wrap>
          <Button disabled={disabled} onClick={() => {
            if (disabled) return;
            restoredSource.current = selected;
            try {
              // Persist our clone before hiding the source from this tab's future recovery list.
              const draft = snapshot(selected.snapshot.value, selected.snapshot.baselineVersion);
              localStorage.setItem(key, JSON.stringify(draft));
              markHandled(selected);
              restoredSource.current = null;
              setState(previous => ({ ...previous, savedAt: draft.savedAt, error: null }));
            } catch {
              setState(previous => ({ ...previous, error: "已恢复到编辑器，但当前窗口的本机副本保存失败。源副本仍保留，下次可再次恢复；请手动保存或复制正文。" }));
            }
            onRestore(selected.snapshot.value, selected.snapshot.baselineVersion);
            closeCandidate(selected, true);
          }}>恢复本机编辑</Button>
          <Button disabled={disabled} onClick={() => {
            if (disabled) return;
            if (selected.key === key) discardCurrent();
            closeCandidate(selected, false);
          }}>{selected.key === key ? "丢弃本机副本" : "忽略此恢复副本"}</Button>
        </Space>
      </Space>} /> : <Space wrap>
        <Typography.Text type="secondary">{current?.savedAt ? `本机恢复副本：${new Date(current.savedAt).toLocaleTimeString()}。` : "未保存输入会保存在本浏览器。"}各窗口分别保留，关闭或退出后可在本机恢复。</Typography.Text>
        {current?.savedAt && <Button size="small" disabled={disabled} onClick={discardCurrent}>删除本机副本</Button>}
        <Button size="small" disabled={disabled} onClick={refreshCandidates}>查找其它恢复副本</Button>
      </Space>}
    </div>,
  };
}
