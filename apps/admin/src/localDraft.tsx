import { formatDateTime } from "./timeZone";
import { useTimeZone } from "./timeZoneContext";
import { Alert, Button, Select, Space, Typography } from "antd";
import { lazy, Suspense, useEffect, useLayoutEffect, useRef, useState } from "react";
import { candidateRevision, draftIdentity, draftKey, draftScope, handledKey, readCandidates, readHandled, snapshot, type DraftCandidate, type DraftIdentity } from "./draftStorage";
import { createDraftWriteQueue, draftValueEquals } from "./draftPersistence";
import { draftCoordination, retainDraftEditor } from "./draftActivity";
import type { StoredDraft } from "./draftManagement";

const LocalDraftManager = lazy(() => import("./components/LocalDraftManager"));

interface DraftState<T> {
  visit: number; key: string; candidates: DraftCandidate<T>[]; selected: string | null;
  reviewing: boolean; savedAt: string | null; error: string | null;
}
const originLabel = { current: "当前窗口", tab: "本标签先前或复制窗口", other: "其它标签", legacy: "旧版恢复副本" };

export function useLocalDraft<T>({ owner, kind, id, ready, disabled, dirty, value, template, baselineVersion, onRestore, readValue, readDirty, identity = draftIdentity() }: {
  owner: string | undefined; kind: "post" | "page"; id: string | null; ready: boolean; disabled: boolean; dirty: boolean;
  value: T; template: T; baselineVersion: number | null; onRestore: (value: T, version: number | null) => void;
  /** Read the form store on departure, including input whose subscription has not rendered yet. */
  readValue?: () => T; readDirty?: () => boolean;
  identity?: DraftIdentity;
}) {
  const timeZone = useTimeZone();
  const storageScope = owner ? draftScope(owner, kind, id) : "";
  const key = owner ? draftKey(storageScope, identity) : "";
  const [state, setState] = useState<DraftState<T>>({ visit: -1, key: "", candidates: [], selected: null, reviewing: false, savedAt: null, error: null });
  const [managerOpen, setManagerOpen] = useState(false);
  const activeKey = useRef("");
  const deletedValue = useRef<{ value: T } | null>(null);
  const adoptedKey = useRef<string | null>(null);
  const handled = useRef<Record<string, string>>({});
  const restoredSource = useRef<DraftCandidate<T> | null>(null);
  const scope = useRef({ key, visit: 0 });
  const [writes] = useState(createDraftWriteQueue);
  const mounted = useRef(true);
  const migratingFrom = useRef<string | null>(null);
  const previousValue = useRef({ value, revision: 0 });
  if (!draftValueEquals(previousValue.current.value, value)) {
    previousValue.current = { value, revision: previousValue.current.revision + 1 };
  }
  if (scope.current.key !== key) {
    scope.current = { key, visit: scope.current.visit + 1 };
    activeKey.current = "";
    restoredSource.current = null;
    if (migratingFrom.current !== key) migratingFrom.current = null;
  }
  const visit = scope.current.visit;
  const revision = previousValue.current.revision;
  type Pending = { key: string; visit: number; value: T; dirty: boolean; baselineVersion: number | null; readValue?: () => T; readDirty?: () => boolean };
  const latest = useRef<Pending | null>(null);
  const lastWritten = useRef<Pending | null>(null);

  function persist(input: Pending) {
    const currentValue = input.readValue?.() ?? input.value;
    const currentDirty = input.readDirty?.() ?? input.dirty;
    if (deletedValue.current && draftValueEquals(deletedValue.current.value, currentValue)) return;
    const previous = lastWritten.current;
    if (previous?.key === input.key && previous.baselineVersion === input.baselineVersion &&
      previous.dirty === currentDirty && draftValueEquals(previous.value, currentValue)) return;
    const currentScope = () => mounted.current && scope.current.key === input.key && scope.current.visit === input.visit;
    try {
      const draft = currentDirty ? { ...snapshot(currentValue, input.baselineVersion), coordination: draftCoordination(input.key) } : null;
      if (draft) localStorage.setItem(input.key, JSON.stringify(draft));
      else localStorage.removeItem(input.key);
      lastWritten.current = { ...input, value: currentValue, dirty: currentDirty };
      if (currentScope()) setState(current => ({ ...current, savedAt: draft?.savedAt ?? null }));
    } catch {
      if (currentScope()) setState(current => ({ ...current, error: "本机恢复副本保存失败，可能是浏览器存储已满或不可用。请手动保存或复制正文。" }));
    }
  }

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
      if (adoptedKey.current === key && dirty && own && draftValueEquals(own.snapshot.value, value)) {
        candidates = candidates.filter(candidate => candidate.key !== key);
      }
      adoptedKey.current = null;
      setState({ visit, key, candidates, selected: candidates[0]?.key ?? null, reviewing: candidates.length > 0, savedAt: own?.snapshot.savedAt ?? null, error: null });
    } catch {
      setState({ visit, key, candidates: [], selected: null, reviewing: false, savedAt: null, error: "无法读取本机恢复副本；当前输入尚未保存到本机，请手动保存或复制正文。" });
    }
  }, [key, visit, ready, template, dirty, revision]);

  useLayoutEffect(() => {
    const eligible = key && ready && state.key === key && state.visit === visit && migratingFrom.current !== key &&
      !(state.reviewing && state.candidates.some(candidate => candidate.key === key)) && !state.error;
    latest.current = eligible ? { key, visit, value, dirty, baselineVersion, readValue, readDirty } : null;
  });
  useLayoutEffect(() => {
    mounted.current = true;
    const flush = () => {
      if (latest.current) writes.enqueue(() => { if (latest.current) persist(latest.current); });
      writes.flush();
    };
    const onVisibility = () => { if (document.visibilityState === "hidden") flush(); };
    window.addEventListener("pagehide", flush);
    window.addEventListener("beforeunload", flush);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      mounted.current = false;
      flush();
      window.removeEventListener("pagehide", flush);
      window.removeEventListener("beforeunload", flush);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [writes]);
  // A scope change flushes the old queue before the next editor can replace its contents.
  useLayoutEffect(() => () => writes.flush(), [key, visit, writes]);
  useLayoutEffect(() => {
    if (!key) return;
    let release = retainDraftEditor(key);
    const depart = () => { release(); };
    const returnToPage = () => {
      release();
      release = retainDraftEditor(key);
      // Another window may safely clean this slot while BFCache has paused its editor.
      // Our previous write no longer proves the stored copy exists after returning.
      lastWritten.current = null;
      if (latest.current) {
        writes.enqueue(() => { if (latest.current) persist(latest.current); });
        writes.flush();
      }
    };
    window.addEventListener("pagehide", depart);
    window.addEventListener("pageshow", returnToPage);
    return () => {
      release();
      window.removeEventListener("pagehide", depart);
      window.removeEventListener("pageshow", returnToPage);
    };
  }, [key]);
  useEffect(() => { setManagerOpen(false); }, [key]);
  useEffect(() => {
    const input = latest.current;
    if (!input) { writes.cancel(); return; }
    if (!dirty) { writes.cancel(); persist(input); }
    else writes.enqueue(() => persist(input));
    // savedAt is feedback, not a new input to persist.
  }, [key, visit, ready, state.key, state.visit, state.reviewing, state.candidates, state.error, dirty, revision, baselineVersion, writes]);

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
  function restoreCandidate(candidate: DraftCandidate<T>) {
    writes.cancel();
    restoredSource.current = candidate;
    try {
      // Persist our clone before hiding the source from this tab's future recovery list.
      const draft = { ...snapshot(candidate.snapshot.value, candidate.snapshot.baselineVersion), coordination: draftCoordination(key) };
      localStorage.setItem(key, JSON.stringify(draft));
      markHandled(candidate);
      restoredSource.current = null;
      setState(previous => ({ ...previous, savedAt: draft.savedAt, error: null }));
    } catch {
      setState(previous => ({ ...previous, error: "已恢复到编辑器，但当前窗口的本机副本保存失败。源副本仍保留，下次可再次恢复；请手动保存或复制正文。" }));
    }
    onRestore(candidate.snapshot.value, candidate.snapshot.baselineVersion);
    closeCandidate(candidate, true);
  }
  function restoreManaged(draft: StoredDraft): string | null {
    if (disabled) return "请等待当前操作完成后再恢复。";
    try {
      // Management previews accept arbitrary stored text; restoration uses the exact form template.
      const candidate = readCandidates(storageScope, identity, template, id === null).find(item => item.key === draft.key);
      if (localStorage.getItem(draft.key) !== draft.raw) return "副本已有更新，请重新查看后恢复。";
      if (!candidate) return "副本字段与当前编辑器不兼容，未覆盖当前输入。";
      restoreCandidate(candidate);
      return null;
    } catch { return "读取恢复副本失败，当前输入仍保留。"; }
  }
  function discardCurrent() {
    if (!key || disabled) return;
    try {
      writes.cancel();
      // This document only owns this exact key; other windows and legacy slots are never removed.
      localStorage.removeItem(key);
      deletedValue.current = { value: readValue?.() ?? value };
      lastWritten.current = null;
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
      writes.cancel();
      latest.current = null;
      localStorage.removeItem(key);
      const nextKey = draftKey(draftScope(owner, kind, nextId), identity);
      adoptedKey.current = nextKey !== key ? nextKey : null;
      migratingFrom.current = nextKey !== key ? key : null;
      // A delayed form subscription must not recreate the just-committed input.
      deletedValue.current = stillDirty ? null : { value: merged };
      let savedAt: string | null = null;
      if (stillDirty) {
        const draft = { ...snapshot(merged, nextVersion), coordination: draftCoordination(nextKey) };
        localStorage.setItem(nextKey, JSON.stringify(draft));
        savedAt = draft.savedAt;
      } else localStorage.removeItem(nextKey);
      lastWritten.current = { key: nextKey, visit, value: merged, dirty: stillDirty, baselineVersion: nextVersion };
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
          options={current.candidates.map(candidate => ({ value: candidate.key, label: `${originLabel[candidate.origin]} · ${formatDateTime(candidate.snapshot.savedAt, timeZone)} · ${candidate.snapshot.baselineVersion === null ? "新草稿" : `v${candidate.snapshot.baselineVersion}`}` }))}
          onChange={selected => setState(previous => ({ ...previous, selected }))} />
        {blocksEditing && <Typography.Text>请先恢复或丢弃当前窗口已有副本，再开始编辑。</Typography.Text>}
        <Typography.Text>找到 {current.candidates.length} 份副本。恢复只填入编辑器，不会发送到服务器。其它窗口的源副本会保留。</Typography.Text>
        {selected.snapshot.baselineVersion !== baselineVersion && <Typography.Text type="warning">服务器版本已变化，恢复后请先对比再保存。</Typography.Text>}
        <Space wrap>
          <Button disabled={disabled} onClick={() => {
            if (disabled) return;
            restoreCandidate(selected);
          }}>恢复本机编辑</Button>
          <Button disabled={disabled} onClick={() => {
            if (disabled) return;
            if (selected.key === key) discardCurrent();
            closeCandidate(selected, false);
          }}>{selected.key === key ? "丢弃本机副本" : "忽略此恢复副本"}</Button>
        </Space>
      </Space>} /> : <Space wrap>
        <Typography.Text type="secondary">{current?.savedAt ? `本机恢复副本：${formatDateTime(current.savedAt, timeZone)}。` : "未保存输入会保存在本浏览器。"}各窗口分别保留，关闭或退出后可在本机恢复。</Typography.Text>
        {current?.savedAt && <Button size="small" disabled={disabled} onClick={discardCurrent}>删除本机副本</Button>}
        <Button size="small" disabled={disabled} onClick={refreshCandidates}>查找其它恢复副本</Button>
      </Space>}
      <div style={{ marginTop: 8 }}><Button size="small" disabled={disabled} onClick={() => setManagerOpen(true)}>管理本机副本</Button></div>
      {managerOpen && owner && <Suspense fallback={null}><LocalDraftManager owner={owner} kind={kind} id={id} disabled={disabled}
        onClose={() => setManagerOpen(false)} onRestore={restoreManaged} onDeleted={removedKey => setState(previous => {
          const candidates = previous.candidates.filter(candidate => candidate.key !== removedKey);
          return { ...previous, candidates, selected: candidates.some(candidate => candidate.key === previous.selected) ? previous.selected : candidates[0]?.key ?? null,
            reviewing: previous.reviewing && candidates.length > 0 };
        })} /></Suspense>}
    </div>,
  };
}
