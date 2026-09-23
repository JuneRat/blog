import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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

vi.mock("../api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../api")>();
  return {
    ...actual,
    api: {
      getPage: vi.fn(),
      createPage: vi.fn(),
      updatePage: vi.fn(),
      publishPage: vi.fn(),
      unpublishPage: vi.fn(),
      deletePage: vi.fn(),
    },
  };
});

import { ApiError, api } from "../api";
import { PageEditScreen } from "./PageEditScreen";
import type { PageDetail } from "../types";

const getPage = vi.mocked(api.getPage);
const createPage = vi.mocked(api.createPage);
const updatePage = vi.mocked(api.updatePage);
const publishPage = vi.mocked(api.publishPage);
const deletePage = vi.mocked(api.deletePage);

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
  render(<PageEditScreen slug="about" />);
  await screen.findByDisplayValue("关于");
}

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => {
  cleanup();
});

describe("PageEditScreen 保存流程", () => {
  it("物理删除必须确认，携带 id 与版本并返回列表", async () => {
    await openExistingPage();
    const confirm = vi.spyOn(window, "confirm").mockReturnValueOnce(false).mockReturnValueOnce(true);
    fireEvent.click(screen.getByRole("button", { name: "永久删除页面" }));
    expect(deletePage).not.toHaveBeenCalled();
    deletePage.mockResolvedValue();
    fireEvent.click(screen.getByRole("button", { name: "永久删除页面" }));
    await waitFor(() => expect(deletePage).toHaveBeenCalledWith("about", "p1", 1));
    await waitFor(() => expect(navigate).toHaveBeenCalledWith("/admin/pages", { replace: true }));
    expect(confirm.mock.calls[0]?.[0]).toContain("无法恢复");
    confirm.mockRestore();
  });

  it("删除遇到旧版本时保留页面并要求重新核对", async () => {
    await openExistingPage();
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    deletePage.mockRejectedValue(new ApiError(409, "版本冲突", "version_conflict"));
    fireEvent.click(screen.getByRole("button", { name: "永久删除页面" }));
    await screen.findByText(/请重新加载并核对最新内容/);
    expect(navigate).not.toHaveBeenCalled();
    getPage.mockResolvedValueOnce(pageDetail({ version: 2 }));
    fireEvent.click(screen.getByRole("button", { name: "重新加载页面" }));
    await screen.findByText("v2");
    expect(screen.queryByRole("button", { name: "重新加载页面" })).toBeNull();
    confirm.mockRestore();
  });
  it("新建页面：提交后跳转到编辑地址", async () => {
    createPage.mockResolvedValue(pageDetail({ slug: "contact", title: "联系" }));
    render(<PageEditScreen slug={null} />);

    fireEvent.change(field(/slug/), { target: { value: "contact" } });
    fireEvent.change(field("标题"), { target: { value: "联系" } });
    fireEvent.change(field(/正文/), { target: { value: "# 联系" } });
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));

    await waitFor(() => expect(createPage).toHaveBeenCalledTimes(1));
    expect(createPage).toHaveBeenCalledWith({
      slug: "contact",
      title: "联系",
      content: "# 联系",
      visibility: "public",
    });
    await waitFor(() => expect(navigate).toHaveBeenCalledWith("/admin/pages/contact/edit"));
  });

  it("保存已存在页面：携带 expected_version 并采用服务器新版本", async () => {
    await openExistingPage();
    updatePage.mockResolvedValue(pageDetail({ version: 2, content: "新正文" }));

    fireEvent.change(field(/正文/), { target: { value: "新正文" } });
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));

    await waitFor(() => expect(updatePage).toHaveBeenCalledTimes(1));
    expect(updatePage).toHaveBeenCalledWith(
      "about",
      expect.objectContaining({ content: "新正文", expected_version: 1 }),
    );
    await screen.findByText(/已保存/);
    expect(screen.getByText("v2")).toBeTruthy();
  });

  it("有未保存改动时发布：先保存再用保存后的版本发布", async () => {
    await openExistingPage();
    updatePage.mockResolvedValue(pageDetail({ version: 2, content: "新正文" }));
    publishPage.mockResolvedValue(
      pageDetail({ version: 3, status: "published", content: "新正文", published_at: "now" }),
    );

    fireEvent.change(field(/正文/), { target: { value: "新正文" } });
    fireEvent.click(screen.getByRole("button", { name: "发布" }));

    await waitFor(() => expect(publishPage).toHaveBeenCalledWith("about", 2));
    expect(updatePage).toHaveBeenCalledWith(
      "about",
      expect.objectContaining({ content: "新正文" }),
    );
    await screen.findByText("已保存并发布。");
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
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));
    // 响应返回前继续输入：这些字符必须保留，且提示仍未保存。
    fireEvent.change(field(/正文/), { target: { value: "A+B" } });
    resolveSave(pageDetail({ version: 2, content: "A" }));

    await screen.findByText(/等待期间的新改动尚未保存/);
    expect((field(/正文/) as HTMLTextAreaElement).value).toBe("A+B");
  });

  it("编辑页后退到新建页时清空全部编辑状态", async () => {
    getPage.mockResolvedValue(
      pageDetail({ status: "published", visibility: "private", version: 7 }),
    );
    const view = render(<PageEditScreen slug="about" />);
    await screen.findByDisplayValue("关于");
    fireEvent.change(field("标题"), { target: { value: "改过的标题" } });

    // 模拟浏览器后退到 /admin/pages/new：App 不按 slug 加 key，复用同一实例。
    view.rerender(<PageEditScreen slug={null} />);

    await waitFor(() => expect((field(/slug/) as HTMLInputElement).value).toBe(""));
    expect((field("标题") as HTMLInputElement).value).toBe("");
    expect((field(/正文/) as HTMLTextAreaElement).value).toBe("");
    expect((field("可见性") as HTMLSelectElement).value).toBe("public");
    expect(screen.getByText("草稿")).toBeTruthy();
    expect(screen.queryByText("已发布")).toBeNull();
    expect(screen.queryByText("v7")).toBeNull();
  });

  it("切换到加载失败的页面时拒绝提交上一篇内容", async () => {
    getPage.mockResolvedValueOnce(pageDetail());
    const view = render(<PageEditScreen slug="about" />);
    await screen.findByDisplayValue("关于");

    // 切到另一页但加载失败：表单里仍是 about 的内容与版本。
    getPage.mockRejectedValueOnce(new ApiError(404, "未找到", "not_found"));
    view.rerender(<PageEditScreen slug="missing" />);
    await screen.findByText(/页面未能加载/);

    // 必须拒绝写入，尤其不能把 about 的 expected_version 发到 missing。
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));
    expect(updatePage).not.toHaveBeenCalled();
    expect(createPage).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "发布" }));
    expect(publishPage).not.toHaveBeenCalled();
  });

  it("加载失败后可用重试按钮恢复编辑", async () => {
    getPage.mockRejectedValueOnce(new ApiError(404, "未找到", "not_found"));
    render(<PageEditScreen slug="missing" />);
    await screen.findByText(/页面未能加载/);

    getPage.mockResolvedValueOnce(pageDetail({ slug: "missing", title: "补回" }));
    fireEvent.click(screen.getByRole("button", { name: "重新加载" }));

    await screen.findByDisplayValue("补回");
    expect(screen.queryByText(/页面未能加载/)).toBeNull();
  });

  it("版本冲突显示冲突横幅；保留路径冲突按普通错误展示", async () => {
    await openExistingPage();
    updatePage.mockRejectedValueOnce(
      new ApiError(409, "版本冲突：内容已被并发修改，请基于最新版本重试", "version_conflict"),
    );
    fireEvent.change(field(/正文/), { target: { value: "本地编辑" } });
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));

    await screen.findByText(/内容已在别处修改/);
    expect((field(/正文/) as HTMLTextAreaElement).value).toBe("本地编辑");

    // 保留路径是 400 invalid_request：不得当成版本冲突给出无效的「仍然覆盖」。
    cleanup();
    await openExistingPage();
    updatePage.mockRejectedValueOnce(
      new ApiError(400, "slug「admin」是系统保留路径，不能用于页面", "invalid_request"),
    );
    fireEvent.change(field(/slug/), { target: { value: "admin" } });
    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));

    await screen.findByText(/系统保留路径/);
    expect(screen.queryByText(/内容已在别处修改/)).toBeNull();
  });
});
