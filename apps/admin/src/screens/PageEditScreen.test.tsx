import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { navigate } = vi.hoisted(() => ({ navigate: vi.fn() }));

vi.mock("../router", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../router")>();
  return { ...actual, navigate };
});

vi.mock("../auth", () => ({
  useAuth: () => ({
    status: "authenticated",
    me: {
      user_id: "u1",
      permissions: [
        "page.read",
        "page.create",
        "page.update",
        "page.publish",
        "page.unpublish",
        "page.delete",
      ],
      csrf_token: "csrf",
      channel: "session",
    },
    providers: [],
    providersLoaded: true,
    logoutError: null,
    refresh: async () => {},
    logout: async () => {},
    goToLogin: async () => {},
  }),
}));

vi.mock("../api/pages", async (load) => {
  const original = await load<typeof import("../api/pages")>();
  return { ...original, pagesApi: { ...original.pagesApi, getPage: vi.fn(), createPage: vi.fn(), updatePage: vi.fn(), publishPage: vi.fn(), schedulePage: vi.fn(), archivePage: vi.fn(), unpublishPage: vi.fn(), trashPage: vi.fn() } };
});

import { ApiError } from "../api/client";
import { pagesApi } from "../api/pages";
import { AdminProviders } from "../providers";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { queryKeys } from "../queryClient";
import { PageEditScreen } from "./PageEditScreen";
import type { PageDetail } from "../types";

/**
 * 编辑屏在真实应用里总处于 AdminProviders 内（主题、zh_CN locale、antd App 上下文）。
 * 测试直接渲染屏幕时补上同样的外壳，否则 `App.useApp()` 的 modal 拿不到上下文，
 * 确认弹窗按钮也会退回英文默认文案。
 */
function renderPage(ui: React.ReactElement): ReturnType<typeof render> {
  return render(<AdminProviders>{ui}</AdminProviders>);
}

function QueryClientProbe({ capture }: { capture: (client: QueryClient) => void }) {
  capture(useQueryClient());
  return null;
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

const getPage = vi.mocked(pagesApi.getPage);
const createPage = vi.mocked(pagesApi.createPage);
const updatePage = vi.mocked(pagesApi.updatePage);
const publishPage = vi.mocked(pagesApi.publishPage);
const trashPage = vi.mocked(pagesApi.trashPage);

function pageDetail(overrides: Partial<PageDetail> = {}): PageDetail {
  return {
    id: "p1",
    slug: "about",
    title: "关于",
    status: "draft",
    visibility: "public",
    version: 1,
    published_at: null,
    updated_at: "2026-09-21 12:00 UTC",
    content: "# 关于",
    ...overrides,
  };
}

function field(
  label: string | RegExp,
): HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement {
  return screen.getByLabelText(label) as
    | HTMLInputElement
    | HTMLTextAreaElement
    | HTMLSelectElement;
}

async function openExistingPage() {
  getPage.mockResolvedValue(pageDetail());
  renderPage(<PageEditScreen id="p1" />);
  await screen.findByDisplayValue("关于");
}

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => {
  cleanup();
});

describe("PageEditScreen 保存流程", () => {
  it("离开保护同步读取表单，输入后立即关闭页面也会确认", async () => {
    await openExistingPage();
    const clean = new Event("beforeunload", { cancelable: true });
    window.dispatchEvent(clean);
    expect(clean.defaultPrevented).toBe(false);

    const dirty = new Event("beforeunload", { cancelable: true });
    act(() => {
      fireEvent.change(field("标题"), { target: { value: "刚输入的标题" } });
      // 与输入处于同一批更新，useWatch 镜像此刻还没重新渲染。
      window.dispatchEvent(dirty);
    });
    expect(dirty.defaultPrevented).toBe(true);
  });

  it("移入回收站必须确认，携带 id 与版本并返回列表", async () => {
    await openExistingPage();
    trashPage.mockResolvedValue(pageDetail({version:2}));
    fireEvent.click(screen.getByRole("button", { name: "移入页面回收站" }));
    // 确认弹窗由 antd 的 modal.confirm 渲染，文案里必须点明不可恢复。
    expect(await screen.findByText(/可从页面回收站恢复/)).toBeTruthy();
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));
    await waitFor(() => expect(trashPage).toHaveBeenCalledWith("p1", 1));
    await waitFor(() => expect(navigate).toHaveBeenCalledWith("/admin/pages", { replace: true }));
  });

  // 取消与确认拆成两条用例：antd 关闭后的弹窗仍留在 DOM 里，
  // 同一条用例里开两次会同时匹配到两个「确定」。
  it("删除确认被取消时不发请求", async () => {
    await openExistingPage();
    fireEvent.click(screen.getByRole("button", { name: "移入页面回收站" }));
    fireEvent.click(await screen.findByRole("button", { name: "取消" }));
    await act(async () => {});
    expect(trashPage).not.toHaveBeenCalled();
  });

  it("删除遇到旧版本时保留页面并要求重新核对", async () => {
    await openExistingPage();
    trashPage.mockRejectedValue(new ApiError(409, "版本冲突", "version_conflict"));
    fireEvent.click(screen.getByRole("button", { name: "移入页面回收站" }));
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));
    await screen.findByText(/请重新加载并核对最新内容/);
    expect(navigate).not.toHaveBeenCalled();
    getPage.mockResolvedValueOnce(pageDetail({ version: 2 }));
    fireEvent.click(screen.getByRole("button", { name: "重新加载页面" }));
    await screen.findByText("v2");
    expect(screen.queryByRole("button", { name: "重新加载页面" })).toBeNull();
  });

  it.each(["success", "conflict", "error"] as const)("旧删除返回%s不会导航、覆盖新建页提示或结束新保存", async outcome => {
    const deletion = deferred<PageDetail>();
    const creation = deferred<PageDetail>();
    trashPage.mockReturnValue(deletion.promise);
    createPage.mockReturnValue(creation.promise);
    getPage.mockResolvedValue(pageDetail());
    let client!: QueryClient;
    const content = (id: string | null) => <AdminProviders>
      <QueryClientProbe capture={current => { client = current; }} />
      <PageEditScreen id={id} />
    </AdminProviders>;
    const view = render(content("p1"));
    await screen.findByDisplayValue("关于");
    client.setQueryData(queryKeys.pages(), []);
    client.setQueryData(queryKeys.pageTrashAll(), []);
    fireEvent.click(screen.getByRole("button", { name: "移入页面回收站" }));
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));
    await waitFor(() => expect(trashPage).toHaveBeenCalledWith("p1", 1));
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    view.rerender(content(null));
    await waitFor(() => expect((field("标题") as HTMLInputElement).value).toBe(""));
    fireEvent.change(field("标题"), { target: { value: "新页自己的输入" } });
    fireEvent.change(field(/正文/), { target: { value: "新页自己的正文" } });
    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    await waitFor(() => expect(createPage).toHaveBeenCalledTimes(1));

    await act(async () => {
      if (outcome === "success") deletion.resolve(pageDetail({ status: "trashed", version: 2 }));
      else deletion.reject(new ApiError(outcome === "conflict" ? 409 : 500, "旧删除返回错误", outcome === "conflict" ? "version_conflict" : "internal_error"));
    });
    expect(navigate).not.toHaveBeenCalled();
    expect((field("标题") as HTMLInputElement).value).toBe("新页自己的输入");
    expect((field(/正文/) as HTMLTextAreaElement).value).toBe("新页自己的正文");
    expect(screen.getByRole("button", { name: "处理中…" }).hasAttribute("disabled")).toBe(true);
    expect(screen.queryByText(/页面已被修改|旧删除返回错误/)).toBeNull();
    expect(screen.queryByRole("button", { name: "重新加载页面" })).toBeNull();
    // The already-sent successful deletion still affects the server's list and trash.
    expect(client.getQueryState(queryKeys.pages())?.isInvalidated).toBe(outcome === "success");
    expect(client.getQueryState(queryKeys.pageTrashAll())?.isInvalidated).toBe(outcome === "success");

    await act(async () => creation.resolve(pageDetail({ id: "new-id", title: "新页自己的输入", content: "新页自己的正文" })));
    expect(navigate).toHaveBeenCalledWith("/admin/pages/new-id/edit");
  });

  it.each(["entity", "leave"] as const)("删除确认延迟到%s之后，不再发送旧页面请求", async target => {
    getPage.mockResolvedValue(pageDetail());
    const view = renderPage(<PageEditScreen id="p1" />);
    await screen.findByDisplayValue("关于");
    fireEvent.click(screen.getByRole("button", { name: "移入页面回收站" }));
    await screen.findByText(/可从页面回收站恢复/);
    if (target === "entity") {
      getPage.mockResolvedValueOnce(pageDetail({ id: "p2", slug: "second", title: "第二页" }));
      view.rerender(<AdminProviders><PageEditScreen id="p2" /></AdminProviders>);
      await screen.findByDisplayValue("第二页");
    } else {
      view.rerender(<AdminProviders><div>已离开编辑器</div></AdminProviders>);
    }
    fireEvent.click(screen.getByRole("button", { name: "确定" }));
    await act(async () => {});
    expect(trashPage).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    if (target === "entity") expect((field("标题") as HTMLInputElement).value).toBe("第二页");
  });

  it("已发送删除在卸载后成功仍刷新缓存，但不导航回列表", async () => {
    const deletion = deferred<PageDetail>();
    trashPage.mockReturnValue(deletion.promise);
    getPage.mockResolvedValue(pageDetail());
    let client!: QueryClient;
    const probe = <QueryClientProbe capture={current => { client = current; }} />;
    const view = renderPage(<>{probe}<PageEditScreen id="p1" /></>);
    await screen.findByDisplayValue("关于");
    client.setQueryData(queryKeys.pages(), []);
    fireEvent.click(screen.getByRole("button", { name: "移入页面回收站" }));
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));
    await waitFor(() => expect(trashPage).toHaveBeenCalledWith("p1", 1));
    view.rerender(<AdminProviders>{probe}<div>已离开编辑器</div></AdminProviders>);
    await act(async () => deletion.resolve(pageDetail({ status: "trashed", version: 2 })));
    expect(client.getQueryState(queryKeys.pages())?.isInvalidated).toBe(true);
    expect(navigate).not.toHaveBeenCalled();
    expect(screen.getByText("已离开编辑器")).toBeTruthy();
  });
  it("新建页面：提交后跳转到编辑地址", async () => {
    createPage.mockResolvedValue(pageDetail({ slug: "contact", title: "联系" }));
    renderPage(<PageEditScreen id={null} />);

    fireEvent.change(field(/slug/), { target: { value: "contact" } });
    fireEvent.change(field("标题"), { target: { value: "联系" } });
    fireEvent.change(field(/正文/), { target: { value: "# 联系" } });
    fireEvent.click(screen.getByRole("button", { name: /保存草稿|保存预约内容|更新已发布内容/ }));

    await waitFor(() => expect(createPage).toHaveBeenCalledTimes(1));
    expect(createPage).toHaveBeenCalledWith({
      slug: "contact",
      title: "联系",
      content: "# 联系",
      visibility: "public",
    });
    await waitFor(() => expect(navigate).toHaveBeenCalledWith("/admin/pages/p1/edit"));
  });

  it("保存已存在页面：携带 expected_version 并采用服务器新版本", async () => {
    await openExistingPage();
    updatePage.mockResolvedValue(pageDetail({ version: 2, content: "新正文" }));

    fireEvent.change(field(/正文/), { target: { value: "新正文" } });
    fireEvent.click(screen.getByRole("button", { name: /保存草稿|保存预约内容|更新已发布内容/ }));

    await waitFor(() => expect(updatePage).toHaveBeenCalledTimes(1));
    expect(updatePage).toHaveBeenCalledWith(
      "p1",
      expect.objectContaining({ content: "新正文", expected_version: 1 }),
    );
    await screen.findByText(/已保存/);
    expect(screen.getByText("v2")).toBeTruthy();
  });

  it("页面改名不切换编辑地址，后续保存继续定位同一 ID", async () => {
    await openExistingPage();
    updatePage.mockResolvedValueOnce(pageDetail({ slug: "about-us", version: 2 }));
    fireEvent.change(field(/slug/), { target: { value: "about-us" } });
    fireEvent.click(screen.getByRole("button", { name: /保存草稿|保存预约内容|更新已发布内容/ }));
    await screen.findByText(/已保存/);
    expect(navigate).not.toHaveBeenCalled();
    expect(updatePage).toHaveBeenLastCalledWith("p1", expect.objectContaining({
      new_slug: "about-us", expected_version: 1,
    }));

    updatePage.mockResolvedValueOnce(pageDetail({ slug: "about-us", title: "改名后的更新", version: 3 }));
    fireEvent.change(field("标题"), { target: { value: "改名后的更新" } });
    fireEvent.click(screen.getByRole("button", { name: /保存草稿|保存预约内容|更新已发布内容/ }));
    await screen.findByText("v3");
    expect(updatePage).toHaveBeenLastCalledWith("p1", expect.objectContaining({
      new_slug: undefined, expected_version: 2, title: "改名后的更新",
    }));
    expect(navigate).not.toHaveBeenCalled();
  });

  it("原 slug 被另一页面占用后，冲突重载仍读取原 ID", async () => {
    await openExistingPage();
    const original = pageDetail({ slug: "about-us", title: "原页面的新版本", version: 2 });
    const replacement = pageDetail({ id: "p2", title: "占用 about 的新页面" });
    getPage.mockImplementation(async (id) => id === original.id ? original : replacement);
    updatePage.mockRejectedValueOnce(new ApiError(409, "版本冲突", "version_conflict"));
    fireEvent.change(field("标题"), { target: { value: "尚未保存的编辑" } });
    fireEvent.click(screen.getByRole("button", { name: /保存草稿|保存预约内容|更新已发布内容/ }));
    await screen.findByText("内容已在别处修改。");
    // The conflict banner arrives before the asynchronous comparison snapshot.
    await waitFor(() => expect((screen.getByRole("button", { name: "重新加载（丢弃本地改动）" }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole("button", { name: "重新加载（丢弃本地改动）" }));
    await screen.findByDisplayValue(original.title);
    expect(getPage).toHaveBeenLastCalledWith(original.id);
    expect((field(/slug/) as HTMLInputElement).value).toBe(original.slug);
    expect(screen.queryByDisplayValue(replacement.title)).toBeNull();
    expect(navigate).not.toHaveBeenCalled();
  });

  it("有未保存改动时发布：先保存再用保存后的版本发布", async () => {
    await openExistingPage();
    updatePage.mockResolvedValue(pageDetail({ version: 2, content: "新正文" }));
    publishPage.mockResolvedValue(
      pageDetail({ version: 3, status: "published", content: "新正文", published_at: "now" }),
    );

    fireEvent.change(field(/正文/), { target: { value: "新正文" } });
    fireEvent.click(screen.getByRole("button", { name: "发布" }));

    await waitFor(() => expect(publishPage).toHaveBeenCalledWith("p1", 2));
    expect(updatePage).toHaveBeenCalledWith(
      "p1",
      expect.objectContaining({ content: "新正文" }),
    );
    await screen.findByText("状态已更新为已发布。");
  });

  it("请求飞行期间的新输入不会被服务器响应覆盖", async () => {
    await openExistingPage();
    let resolveSave: (page: PageDetail) => void = () => {};
    updatePage.mockReturnValue(
      new Promise<PageDetail>((resolve) => {
        resolveSave = resolve;
      }),
    );

    fireEvent.change(field(/正文/), { target: { value: "A" } });
    fireEvent.click(screen.getByRole("button", { name: /保存草稿|保存预约内容|更新已发布内容/ }));
    // antd Form 的 onFinish 在校验之后才触发；必须等请求真正发出，
    // 才是「请求飞行期间」继续输入。
    await waitFor(() => expect(updatePage).toHaveBeenCalledTimes(1));
    fireEvent.change(field(/正文/), { target: { value: "A+B" } });
    resolveSave(pageDetail({ version: 2, content: "A" }));

    await screen.findByText(/等待期间的新改动尚未保存/);
    expect((field(/正文/) as HTMLTextAreaElement).value).toBe("A+B");
  });

  it("编辑页后退到新建页时清空全部编辑状态", async () => {
    getPage.mockResolvedValue(
      pageDetail({ status: "published", visibility: "private", version: 7 }),
    );
    const view = renderPage(<PageEditScreen id="p1" />);
    await screen.findByDisplayValue("关于");
    fireEvent.change(field("标题"), { target: { value: "改过的标题" } });

    // 模拟浏览器后退到 /admin/pages/new：App 不按 ID 加 key，复用同一实例。
    view.rerender(
      <AdminProviders>
        <PageEditScreen id={null} />
      </AdminProviders>,
    );

    await waitFor(() => expect((field(/slug/) as HTMLInputElement).value).toBe(""));
    expect((field("标题") as HTMLInputElement).value).toBe("");
    expect((field(/正文/) as HTMLTextAreaElement).value).toBe("");
    // 可见性从原生 select 换成 antd Select：断言选中项文案（antd Select 不是原生控件）。
    expect(screen.getByTitle("公开")).toBeTruthy();
    expect(screen.getByText("草稿")).toBeTruthy();
    expect(screen.queryByText("已发布")).toBeNull();
    expect(screen.queryByText("v7")).toBeNull();
  });

  it("切换到加载失败的页面时拒绝提交上一篇内容", async () => {
    getPage.mockResolvedValueOnce(pageDetail());
    const view = renderPage(<PageEditScreen id="p1" />);
    await screen.findByDisplayValue("关于");

    // 切到另一页但加载失败：表单里仍是 about 的内容与版本。
    getPage.mockRejectedValueOnce(new ApiError(404, "未找到", "not_found"));
    view.rerender(
      <AdminProviders>
        <PageEditScreen id="missing" />
      </AdminProviders>,
    );
    await screen.findByText(/页面未能加载/);

    // 必须拒绝写入，尤其不能把 about 的 expected_version 发到 missing。
    fireEvent.click(screen.getByRole("button", { name: /保存草稿|保存预约内容|更新已发布内容/ }));
    expect(updatePage).not.toHaveBeenCalled();
    expect(createPage).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "发布" }));
    expect(publishPage).not.toHaveBeenCalled();
  });

  it("加载失败后可用重试按钮恢复编辑", async () => {
    getPage.mockRejectedValueOnce(new ApiError(404, "未找到", "not_found"));
    renderPage(<PageEditScreen id="missing" />);
    await screen.findByText(/页面未能加载/);

    getPage.mockResolvedValueOnce(pageDetail({ id: "missing", slug: "missing", title: "补回" }));
    fireEvent.click(screen.getByRole("button", { name: "重新加载" }));

    await screen.findByDisplayValue("补回");
    expect(screen.queryByText(/页面未能加载/)).toBeNull();
  });

  it("版本冲突显示冲突横幅并保留本地输入", async () => {
    await openExistingPage();
    updatePage.mockRejectedValueOnce(
      new ApiError(409, "版本冲突：内容已被并发修改，请基于最新版本重试", "version_conflict"),
    );
    fireEvent.change(field(/正文/), { target: { value: "本地编辑" } });
    fireEvent.click(screen.getByRole("button", { name: /保存草稿|保存预约内容|更新已发布内容/ }));

    await screen.findByText(/内容已在别处修改/);
    expect((field(/正文/) as HTMLTextAreaElement).value).toBe("本地编辑");

  });

  it("保留路径冲突按普通错误展示，不提供无效覆盖", async () => {
    // This is an independent edit session, not a remount that resumes the preceding local draft.
    await openExistingPage();
    updatePage.mockRejectedValueOnce(
      new ApiError(400, "slug「admin」是系统保留路径，不能用于页面", "invalid_request"),
    );
    fireEvent.change(field(/slug/), { target: { value: "admin" } });
    fireEvent.click(screen.getByRole("button", { name: /保存草稿|保存预约内容|更新已发布内容/ }));

    await screen.findByText(/系统保留路径/);
    expect(screen.queryByText(/内容已在别处修改/)).toBeNull();
  });
});
