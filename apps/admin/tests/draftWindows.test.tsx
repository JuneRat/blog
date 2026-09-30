import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { AdminProviders } from "../src/providers";
import { useLocalDraft } from "../src/localDraft";
import { draftIdentity, draftKey, draftScope, snapshot, type DraftIdentity } from "../src/draftStorage";
import { DRAFT_WRITE_INTERVAL_MS } from "../src/draftPersistence";

const a: DraftIdentity = { tabId: "tab-a", writerId: "document-a" };
const b: DraftIdentity = { tabId: "tab-b", writerId: "document-b" };
const scope = draftScope("owner", "page", "page-id");
const template = { content: "", title: "" };
function Harness({ identity, name, serverVersion = 2 }: { identity: DraftIdentity; name: string; serverVersion?: number }) {
  const [value, setValue] = useState(template);
  const [baseline, setBaseline] = useState(template);
  const [version, setVersion] = useState(serverVersion);
  const draft = useLocalDraft({ owner: "owner", kind: "page", id: "page-id", identity, ready: true, disabled: false,
    dirty: JSON.stringify(value) !== JSON.stringify(baseline), value, template, baselineVersion: version,
    onRestore: (value, originalVersion) => { setValue(value); setVersion(originalVersion!); },
  });
  return <section aria-label={name}>
    <input aria-label={`${name} content`} value={value.content} onChange={event => setValue({ ...value, content: event.target.value })} />
    <span>提交版本 {version}</span>
    {draft.panel}
    <button onClick={() => { draft.saved("page-id", version + 1, value, false); setBaseline(value); setVersion(version + 1); }}>保存服务器</button>
  </section>;
}
function mount(identity: DraftIdentity, name: string) { return render(<AdminProviders><Harness identity={identity} name={name} /></AdminProviders>); }
function section(name: string) { return within(screen.getByRole("region", { name })); }
function change(name: string, content: string) { fireEvent.change(screen.getByLabelText(`${name} content`), { target: { value: content } }); }
function read(identity: DraftIdentity) { return JSON.parse(localStorage.getItem(draftKey(scope, identity))!); }
afterEach(() => { cleanup(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

it("两个窗口分别保留输入，A保存和删除都不移除B副本", async () => {
  mount(a, "A"); mount(b, "B");
  change("A", "A 的编辑"); change("B", "B 的编辑");
  await section("A").findByRole("button", { name: "删除本机副本" });
  await section("B").findByRole("button", { name: "删除本机副本" });
  expect(read(a).value.content).toBe("A 的编辑"); expect(read(b).value.content).toBe("B 的编辑");
  const bSnapshot = localStorage.getItem(draftKey(scope, b));
  fireEvent.click(section("A").getByRole("button", { name: "保存服务器" }));
  expect(localStorage.getItem(draftKey(scope, a))).toBeNull();
  expect(localStorage.getItem(draftKey(scope, b))).toBe(bSnapshot);
  change("A", "A 再次编辑");
  fireEvent.click(await section("A").findByRole("button", { name: "删除本机副本" }));
  expect(localStorage.getItem(draftKey(scope, a))).toBeNull(); expect(localStorage.getItem(draftKey(scope, b))).toBe(bSnapshot);
  expect((screen.getByLabelText("B content") as HTMLInputElement).value).toBe("B 的编辑");
});

it("复制标签继承同一tabId时，不同documentId仍分别写入", async () => {
  const duplicated = { tabId: a.tabId, writerId: "duplicated-document" };
  mount(a, "A"); mount(duplicated, "Copy");
  change("A", "原标签编辑"); change("Copy", "复制标签编辑");
  await section("Copy").findByRole("button", { name: "删除本机副本" });
  expect(read(a).value.content).toBe("原标签编辑"); expect(read(duplicated).value.content).toBe("复制标签编辑");
  fireEvent.click(section("A").getByRole("button", { name: "保存服务器" }));
  expect(read(duplicated).value.content).toBe("复制标签编辑");
});

it("多个槽可选择恢复，保留源窗口副本及原CAS版本", async () => {
  const ownOld = { tabId: a.tabId, writerId: "previous-document" };
  localStorage.setItem(draftKey(scope, ownOld), JSON.stringify(snapshot({ ...template, content: "本标签先前编辑" }, 1)));
  localStorage.setItem(draftKey(scope, b), JSON.stringify(snapshot({ ...template, content: "其它窗口编辑" }, 2)));
  const source = localStorage.getItem(draftKey(scope, b));
  mount(a, "A"); await screen.findByText("找到 2 份副本。恢复只填入编辑器，不会发送到服务器。其它窗口的源副本会保留。");
  expect((screen.getByLabelText("A content") as HTMLInputElement).value).toBe("");
  fireEvent.mouseDown(screen.getByRole("combobox", { name: "选择本机恢复副本" }));
  const option = await screen.findByText(/^其它标签 ·/);
  fireEvent.click(option);
  fireEvent.click(screen.getByRole("button", { name: "恢复本机编辑" }));
  await waitFor(() => expect(read(a).value.content).toBe("其它窗口编辑"));
  expect(localStorage.getItem(draftKey(scope, b))).toBe(source);
  fireEvent.click(screen.getByRole("button", { name: "查找其它恢复副本" }));
  fireEvent.click(screen.getByRole("button", { name: "恢复本机编辑" }));
  expect(screen.getByText("提交版本 1")).toBeTruthy();
  expect(read(a).baselineVersion).toBe(1);
  expect(read(a).value.content).toBe("本标签先前编辑");
});

it("恢复并保存后刷新不再复活已处理外部副本，源窗口仍可继续编辑", async () => {
  localStorage.setItem(draftKey(scope, b), JSON.stringify(snapshot({ ...template, content: "B 原输入" }, 1)));
  const mounted = mount(a, "A"); await screen.findByText("发现本机未保存的编辑");
  fireEvent.click(screen.getByRole("button", { name: "恢复本机编辑" }));
  fireEvent.click(screen.getByRole("button", { name: "保存服务器" }));
  mounted.unmount();
  const refreshed = { tabId: a.tabId, writerId: "document-a-refresh" };
  mount(refreshed, "A");
  await act(async () => {});
  expect(screen.queryByText("发现本机未保存的编辑")).toBeNull();
  expect(read(b).value.content).toBe("B 原输入");
  localStorage.setItem(draftKey(scope, b), JSON.stringify(snapshot({ ...template, content: "B 后续编辑" }, 1)));
  fireEvent.click(screen.getByRole("button", { name: "查找其它恢复副本" }));
  await screen.findByText("发现本机未保存的编辑");
});

it("忽略外部或旧v1副本只记录本标签选择，不删除共享源数据", async () => {
  const legacyKey = `blog:local-draft:v1:${scope}`;
  const legacy = { schema: 1, savedAt: new Date().toISOString(), baselineVersion: 1, value: { ...template, content: "v1遗留编辑" } };
  localStorage.setItem(legacyKey, JSON.stringify(legacy));
  const mounted = mount(a, "A"); await screen.findByText("发现本机未保存的编辑");
  fireEvent.click(screen.getByRole("button", { name: "忽略此恢复副本" }));
  expect(JSON.parse(localStorage.getItem(legacyKey)!)).toEqual(legacy);
  mounted.unmount(); mount({ tabId: a.tabId, writerId: "refresh-a" }, "A"); await act(async () => {});
  expect(screen.queryByText("发现本机未保存的编辑")).toBeNull();
  cleanup(); mount(b, "B"); await screen.findByText("发现本机未保存的编辑");
  fireEvent.click(screen.getByRole("button", { name: "恢复本机编辑" }));
  expect(read(b).schema).toBe(2); expect(read(b).baselineVersion).toBe(1); expect(read(b).value.content).toBe("v1遗留编辑");
  expect(JSON.parse(localStorage.getItem(legacyKey)!)).toEqual(legacy);
});

it("HTTP局域网缺少randomUUID时仍可生成标签身份和写入副本", async () => {
  vi.stubGlobal("crypto", { getRandomValues: crypto.getRandomValues.bind(crypto) });
  const identity = draftIdentity();
  expect(identity.tabId.length).toBeGreaterThan(0);
  mount(identity, "HTTP"); change("HTTP", "局域网写作");
  await screen.findByRole("button", { name: "删除本机副本" });
  expect(read(identity).value.content).toBe("局域网写作");
});

it("仅有外部候选时当前新输入继续保存，刷新可发现自己的输入", async () => {
  localStorage.setItem(draftKey(scope, b), JSON.stringify(snapshot({ ...template, content: "B旧副本" }, 1)));
  const mounted = mount(a, "A"); await screen.findByText("发现本机未保存的编辑");
  change("A", "我没有恢复，直接写的新稿");
  await waitFor(() => expect(read(a).value.content).toBe("我没有恢复，直接写的新稿"));
  expect(read(b).value.content).toBe("B旧副本");
  mounted.unmount();
  mount({ tabId: a.tabId, writerId: "a-after-refresh" }, "A");
  await screen.findByText("发现本机未保存的编辑");
  fireEvent.click(screen.getByRole("button", { name: "恢复本机编辑" }));
  expect((screen.getByLabelText("A content") as HTMLInputElement).value).toBe("我没有恢复，直接写的新稿");
});

it("恢复克隆写入失败不隐藏源修订，刷新后仍可再次恢复", async () => {
  localStorage.setItem(draftKey(scope, b), JSON.stringify(snapshot({ ...template, content: "不能丢失的源副本" }, 1)));
  const mounted = mount(a, "A"); await screen.findByText("发现本机未保存的编辑");
  const setItem = Storage.prototype.setItem;
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(function(this: Storage, key, value) {
    if (key === draftKey(scope, a)) throw new DOMException("quota exceeded", "QuotaExceededError");
    return setItem.call(this, key, value);
  });
  fireEvent.click(screen.getByRole("button", { name: "恢复本机编辑" }));
  await screen.findByText(/已恢复到编辑器，但当前窗口的本机副本保存失败/);
  expect((screen.getByLabelText("A content") as HTMLInputElement).value).toBe("不能丢失的源副本");
  expect(read(b).value.content).toBe("不能丢失的源副本");
  mounted.unmount(); vi.restoreAllMocks();
  mount({ tabId: a.tabId, writerId: "a-after-quota-failure" }, "A");
  await screen.findByText("发现本机未保存的编辑");
  fireEvent.click(screen.getByRole("button", { name: "恢复本机编辑" }));
  expect((screen.getByLabelText("A content") as HTMLInputElement).value).toBe("不能丢失的源副本");
});

it("连续长文输入按有界窗口合并，离页补写最新内容且不重复写盘", () => {
  vi.useFakeTimers();
  const writes = vi.spyOn(Storage.prototype, "setItem");
  mount(a, "A");
  const body = "长文".repeat(100_000);
  for (let index = 0; index < 8; index += 1) {
    change("A", `${body}${index}`);
    act(() => { vi.advanceTimersByTime(DRAFT_WRITE_INTERVAL_MS / 8); });
  }
  expect(read(a).value.content).toBe(`${body}7`);
  const ownWrites = () => writes.mock.calls.filter(([key]) => key === draftKey(scope, a));
  expect(ownWrites()).toHaveLength(1);
  act(() => { vi.advanceTimersByTime(DRAFT_WRITE_INTERVAL_MS * 2); });
  expect(ownWrites()).toHaveLength(1);
  change("A", `${body}最后输入`);
  act(() => { window.dispatchEvent(new Event("pagehide")); });
  expect(read(a).value.content).toBe(`${body}最后输入`);
  expect(ownWrites()).toHaveLength(2);
  act(() => { vi.advanceTimersByTime(DRAFT_WRITE_INTERVAL_MS); });
  expect(ownWrites()).toHaveLength(2);
});

it("离页直接读取表单store，保存和删除不会被待写任务复活", () => {
  vi.useFakeTimers();
  let store = template;
  function StoreHarness() {
    const draft = useLocalDraft({ owner: "owner", kind: "page", id: "page-id", identity: a, ready: true, disabled: false,
      dirty: false, value: template, template, baselineVersion: 2, onRestore: () => {},
      readValue: () => store, readDirty: () => store !== template,
    });
    return <>{draft.panel}<button onClick={() => draft.saved("page-id", 3, store, false)}>提交</button></>;
  }
  render(<AdminProviders><StoreHarness /></AdminProviders>);
  store = { ...template, content: "订阅尚未渲染的输入" };
  act(() => { window.dispatchEvent(new Event("beforeunload")); });
  expect(read(a).value.content).toBe(store.content);
  fireEvent.click(screen.getByRole("button", { name: "提交" }));
  act(() => { vi.advanceTimersByTime(DRAFT_WRITE_INTERVAL_MS); });
  expect(localStorage.getItem(draftKey(scope, a))).toBeNull();
  store = template;
});

it("切换身份与实体时补写旧槽，创建ID迁移不会复活new槽", () => {
  vi.useFakeTimers();
  function ScopeHarness({ owner, id, content }: { owner: string; id: string | null; content: string }) {
    const value = { ...template, content };
    const draft = useLocalDraft({ owner, kind: "page", id, identity: a, ready: true, disabled: false,
      dirty: true, value, template, baselineVersion: id === null ? null : 2, onRestore: () => {},
    });
    return <>{draft.panel}<button onClick={() => draft.saved("created", 3, value, true)}>获得ID</button></>;
  }
  const view = render(<AdminProviders><ScopeHarness owner="owner" id="page-id" content="原身份编辑" /></AdminProviders>);
  view.rerender(<AdminProviders><ScopeHarness owner="another" id="another-id" content="新身份编辑" /></AdminProviders>);
  expect(read(a).value.content).toBe("原身份编辑");
  act(() => { vi.advanceTimersByTime(DRAFT_WRITE_INTERVAL_MS); });
  expect(JSON.parse(localStorage.getItem(draftKey(draftScope("another", "page", "another-id"), a))!).value.content).toBe("新身份编辑");
  view.rerender(<AdminProviders><ScopeHarness owner="owner" id={null} content="创建期间继续输入" /></AdminProviders>);
  fireEvent.click(screen.getByRole("button", { name: "获得ID" }));
  act(() => { window.dispatchEvent(new Event("pagehide")); vi.advanceTimersByTime(DRAFT_WRITE_INTERVAL_MS); });
  expect(localStorage.getItem(draftKey(draftScope("owner", "page", null), a))).toBeNull();
  expect(JSON.parse(localStorage.getItem(draftKey(draftScope("owner", "page", "created"), a))!).value.content).toBe("创建期间继续输入");
});
