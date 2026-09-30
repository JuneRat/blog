import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { draftActivities, draftCoordination, retainDraftEditor } from "../src/draftActivity";
import { listStoredDrafts, removeStoredDraft } from "../src/draftManagement";
import { draftKey, draftScope, snapshot } from "../src/draftStorage";
import { useLocalDraft } from "../src/localDraft";
import { AdminProviders } from "../src/providers";

/** A small native-API substitute; business code still uses query/request rather than test hooks. */
class TestLocks {
  private held = new Set<string>();
  async query() { return { held: [...this.held].map(name => ({ name, mode: "exclusive", clientId: "test" })), pending: [] }; }
  async request(name: string, options: LockOptions, callback: (lock: Lock | null) => unknown) {
    if (options.signal?.aborted) throw new DOMException("Aborted", "AbortError");
    if (this.held.has(name)) {
      if (options.ifAvailable) return callback(null);
      throw new Error("Unexpected competing editor in this test");
    }
    this.held.add(name);
    try { return await callback({ name, mode: "exclusive" } as Lock); }
    finally { this.held.delete(name); }
  }
}
const owner = "owner:one";
const identity = { tabId: "my-tab", writerId: "my-document" };
const foreign = { tabId: "old-tab", writerId: "old-document" };
const scope = draftScope(owner, "page", "page-id");
const foreignKey = draftKey(scope, foreign);
const template = { title: "", content: "" };
function store(key = foreignKey, value: unknown = { title: "旧副本", content: "旧正文" }, coordinated = true) {
  const raw = JSON.stringify({ ...snapshot(value, 2), ...(coordinated ? { coordination: "web-lock-v1" } : {}) });
  localStorage.setItem(key, raw);
  return raw;
}
function installLocks() {
  const locks = new TestLocks();
  vi.spyOn(navigator, "locks", "get").mockReturnValue(locks as unknown as LockManager);
  return locks;
}
beforeEach(() => {
  Object.defineProperty(navigator, "locks", { configurable: true, get: () => undefined });
  localStorage.clear(); sessionStorage.clear();
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("本机副本管理的删除保护", () => {
  it("只列当前账号，保留旧协议和损坏副本，并估算占用", async () => {
    installLocks();
    const raw = store();
    store(draftKey(draftScope("other-account", "page", "page-id"), foreign));
    const oldV2 = draftKey(scope, { tabId: "v2", writerId: "before-upgrade" });
    store(oldV2, template, false);
    const legacy = `blog:local-draft:v1:${scope}`;
    localStorage.setItem(legacy, JSON.stringify({ schema: 1, savedAt: "2026-09-30T01:00:00Z", baselineVersion: 1, value: template }));
    const corrupt = draftKey(scope, { tabId: "bad", writerId: "bad" });
    localStorage.setItem(corrupt, "损坏 JSON");
    const copies = await listStoredDrafts(owner);
    expect(copies).toHaveLength(4);
    expect(copies.find(draft => draft.key === foreignKey)).toMatchObject({ title: "旧副本", baselineVersion: 2, activity: "idle", bytes: (raw.length + foreignKey.length) * 2 });
    for (const key of [oldV2, legacy, corrupt]) {
      const draft = copies.find(item => item.key === key)!;
      expect(draft.activity).toBe("unknown");
      expect(await removeStoredDraft(draft)).toBe("unknown");
      expect(localStorage.getItem(key)).not.toBeNull();
    }
  });

  it("持锁期间不删除，最后一个编辑器离开后才可清理", async () => {
    installLocks(); store();
    const releaseA = retainDraftEditor(foreignKey);
    const releaseB = retainDraftEditor(foreignKey);
    expect(draftCoordination(foreignKey)).toBe("web-lock-v1");
    const [draft] = await listStoredDrafts(owner);
    expect(draft!.activity).toBe("active");
    expect(await removeStoredDraft(draft!)).toBe("active");
    releaseA();
    expect((await draftActivities([foreignKey])).get(foreignKey)).toBe("active");
    releaseB();
    await waitFor(async () => expect((await draftActivities([foreignKey])).get(foreignKey)).toBe("idle"));
    const [inactive] = await listStoredDrafts(owner);
    expect(await removeStoredDraft(inactive!)).toBe("deleted");
    expect(localStorage.getItem(foreignKey)).toBeNull();
  });

  it("确认期间出现更晚更新时保留新快照", async () => {
    installLocks(); store();
    const [draft] = await listStoredDrafts(owner);
    const newer = store(foreignKey, { title: "后来输入", content: "新正文" });
    expect(await removeStoredDraft(draft!)).toBe("changed");
    expect(localStorage.getItem(foreignKey)).toBe(newer);
  });

  it("列表显示闲置后其它窗口重新持锁，删除操作仍会拒绝", async () => {
    const locks = installLocks(); store();
    const [draft] = await listStoredDrafts(owner);
    let release = () => {};
    const held = locks.request(`blog:draft-editor:v1:${foreignKey}`, {}, () => new Promise<void>(resolve => { release = resolve; }));
    expect(await removeStoredDraft(draft!)).toBe("active");
    expect(localStorage.getItem(foreignKey)).toBe(draft!.raw);
    release(); await held;
  });

  it("没有锁API或获取被拒绝时，不能宣称副本已受协议协调", async () => {
    store(foreignKey, template, false);
    const release = retainDraftEditor(foreignKey);
    expect(draftCoordination(foreignKey)).toBeUndefined();
    release();
    const [draft] = await listStoredDrafts(owner);
    expect(draft!.activity).toBe("unknown");
    expect(await removeStoredDraft(draft!)).toBe("unknown");
    const denied = { request: () => Promise.reject(new Error("Denied")), query: () => Promise.reject(new Error("Denied")) };
    vi.spyOn(navigator, "locks", "get").mockReturnValue(denied as unknown as LockManager);
    const releaseDenied = retainDraftEditor(foreignKey);
    await Promise.resolve();
    expect(draftCoordination(foreignKey)).toBeUndefined();
    expect(JSON.parse(localStorage.getItem(foreignKey)!).coordination).toBeUndefined();
    releaseDenied();
  });
});

function Harness() {
  const [value, setValue] = useState(template);
  const draft = useLocalDraft({ owner, kind: "page", id: "page-id", identity, ready: true, disabled: false,
    dirty: value.title !== "" || value.content !== "", value, template, baselineVersion: 2, onRestore: value => setValue(value) });
  return <><input aria-label="编辑器标题" value={value.title} onChange={event => setValue({ ...value, title: event.target.value })} />{draft.panel}</>;
}
function mount() { return render(<AdminProviders><Harness /></AdminProviders>); }
async function openManager() {
  fireEvent.click(screen.getByRole("button", { name: "管理本机副本" }));
  // rc-component's test-mode IDs collide with the existing recovery Select.
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByText("管理本机恢复副本")).toBeTruthy();
  await screen.findByRole("button", { name: "恢复到当前编辑器" });
}

describe("本机副本管理的交互", () => {
  it("无锁API也能查看并恢复旧副本；源副本保留，删除受保护", async () => {
    const source = store(foreignKey, { title: "旧标题", content: "旧正文" }, false);
    mount(); await openManager();
    expect(screen.getByRole("button", { name: "删除副本：旧标题" }).hasAttribute("disabled")).toBe(true);
    expect(screen.getByLabelText("副本正文预览").textContent).toBe("旧正文");
    fireEvent.click(screen.getByRole("button", { name: "恢复到当前编辑器" }));
    fireEvent.click(await screen.findByRole("button", { name: "恢复副本" }));
    await waitFor(() => expect((screen.getByLabelText("编辑器标题") as HTMLInputElement).value).toBe("旧标题"));
    expect(localStorage.getItem(foreignKey)).toBe(source);
    const own = JSON.parse(localStorage.getItem(draftKey(scope, identity))!);
    expect(own.value.content).toBe("旧正文");
    expect(own.coordination).toBeUndefined();
  });

  it("预览可读但字段不符合当前模板时，拒绝覆盖编辑器", async () => {
    store(foreignKey, { title: "形状错误", content: 123 }, false);
    mount(); await openManager();
    fireEvent.click(screen.getByRole("button", { name: "恢复到当前编辑器" }));
    fireEvent.click(await screen.findByRole("button", { name: "恢复副本" }));
    await screen.findByText("副本字段与当前编辑器不兼容，未覆盖当前输入。");
    expect((screen.getByLabelText("编辑器标题") as HTMLInputElement).value).toBe("");
  });

  it("确认恢复前源副本更新时，保留当前输入并要求重新核对", async () => {
    store(); mount(); await openManager();
    fireEvent.click(screen.getByRole("button", { name: "恢复到当前编辑器" }));
    const newer = store(foreignKey, { title: "源的新标题", content: "源的新正文" });
    fireEvent.click(await screen.findByRole("button", { name: "恢复副本" }));
    await screen.findByText("副本已有更新，请重新查看后恢复。");
    expect((screen.getByLabelText("编辑器标题") as HTMLInputElement).value).toBe("");
    expect(localStorage.getItem(foreignKey)).toBe(newer);
  });

  it("明确确认才删除闲置副本，当前编辑器输入不受影响", async () => {
    installLocks(); store(); mount(); await openManager();
    fireEvent.click(screen.getByRole("button", { name: "删除副本：旧副本" }));
    expect(localStorage.getItem(foreignKey)).not.toBeNull();
    fireEvent.click(await screen.findByRole("button", { name: "删除副本" }));
    await waitFor(() => expect(localStorage.getItem(foreignKey)).toBeNull());
    await screen.findByText("当前账号没有本机恢复副本");
    expect((screen.getByLabelText("编辑器标题") as HTMLInputElement).value).toBe("");
  });

  it("BFCache暂停期间副本被清理，返回后立即补写输入并重新保护", async () => {
    installLocks();
    const mounted = mount();
    fireEvent.change(screen.getByLabelText("编辑器标题"), { target: { value: "正在编辑" } });
    act(() => { window.dispatchEvent(new Event("pagehide")); });
    const ownKey = draftKey(scope, identity);
    await waitFor(async () => expect((await draftActivities([ownKey])).get(ownKey)).toBe("idle"));
    const [paused] = await listStoredDrafts(owner);
    expect(await removeStoredDraft(paused!)).toBe("deleted");
    expect(localStorage.getItem(ownKey)).toBeNull();
    act(() => { window.dispatchEvent(new Event("pageshow")); });
    expect(JSON.parse(localStorage.getItem(ownKey)!).value.title).toBe("正在编辑");
    expect(screen.getByText(/本机恢复副本：/)).toBeTruthy();
    expect((await draftActivities([ownKey])).get(ownKey)).toBe("active");
    // No further input is needed for another departure to retain the same current content.
    act(() => { window.dispatchEvent(new Event("pagehide")); });
    expect(JSON.parse(localStorage.getItem(ownKey)!).value.title).toBe("正在编辑");
    mounted.unmount();
    await waitFor(async () => expect((await draftActivities([ownKey])).get(ownKey)).toBe("idle"));
    expect(JSON.parse(localStorage.getItem(ownKey)!).value.title).toBe("正在编辑");
  });
});
