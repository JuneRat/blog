// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError, categoryApi } from "../src/api";
import { paths } from "../src/router";
import type { CategorySummary } from "../src/types";

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    status: "authenticated",
    me: { permissions: ["category.manage"] },
  }),
}));

vi.mock("../src/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("../src/api")>();
  return {
    ...original,
    categoryApi: { list: vi.fn(), create: vi.fn(), update: vi.fn(), remove: vi.fn() },
  };
});

const tech: CategorySummary = {
  id: "cat-tech", slug: "tech", name: "技术", parent_id: null,
  description: null, version: 1, pub_post_count: 2,
};
const rust: CategorySummary = {
  id: "cat-rust", slug: "rust", name: "Rust", parent_id: "cat-tech",
  description: null, version: 1, pub_post_count: 0,
};
/** 创建用例里后端重取后才会出现的分类。 */
const life: CategorySummary = {
  id: "cat-life", slug: "life", name: "生活", parent_id: "cat-tech",
  description: null, version: 1, pub_post_count: 0,
};

beforeEach(() => {
  vi.resetAllMocks();
  window.history.replaceState(null, "", paths.categories);
  vi.mocked(categoryApi.list).mockResolvedValue([rust, tech]);
});
afterEach(cleanup);

/**
 * 选中 antd Select 的选项。
 *
 * 迁移后「父分类」「移动到…」都是 antd Select：它没有原生 `<select>`，
 * `fireEvent.change` 只改输入框里的过滤文字、不会产生选中值，
 * 必须先在 combobox 上 mouseDown 展开下拉，再点中带 `title` 的选项。
 */
async function selectOption(labelText: string, optionTitle: string): Promise<void> {
  fireEvent.mouseDown(screen.getByLabelText(labelText));
  fireEvent.click(await screen.findByTitle(optionTitle));
}

describe("分类管理屏", () => {
  it("树形列出目录并创建子分类", async () => {
    vi.mocked(categoryApi.create).mockResolvedValue(tech);
    render(<App />);
    await waitFor(() => expect(screen.getByRole("link", { name: "Rust" })).toBeTruthy());

    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "生活" } });
    fireEvent.change(screen.getByLabelText("slug"), { target: { value: "life" } });
    await selectOption("父分类", "技术");
    // 创建后的重取会拿到含新分类的目录：用它证明列表刷新了，而不是去数 list 调了几次。
    vi.mocked(categoryApi.list).mockResolvedValue([rust, tech, life]);
    fireEvent.click(screen.getByRole("button", { name: "创建分类" }));

    await waitFor(() =>
      expect(categoryApi.create).toHaveBeenCalledWith({ name: "生活", slug: "life", parent: "tech" }),
    );
    expect(await screen.findByRole("link", { name: "生活" })).toBeTruthy();
  });

  it("父分类默认根分类：留空创建时不带 parent", async () => {
    vi.mocked(categoryApi.create).mockResolvedValue(tech);
    render(<App />);
    await waitFor(() => expect(screen.getByRole("link", { name: "Rust" })).toBeTruthy());
    // 默认选中「（根分类）」，不是空占位。
    expect(screen.getByText("（根分类）")).toBeTruthy();

    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "生活" } });
    fireEvent.change(screen.getByLabelText("slug"), { target: { value: "life" } });
    fireEvent.click(screen.getByRole("button", { name: "创建分类" }));

    await waitFor(() =>
      expect(categoryApi.create).toHaveBeenCalledWith({ name: "生活", slug: "life", parent: undefined }),
    );
  });

  it("移动成环展示服务端错误文案", async () => {
    vi.mocked(categoryApi.update).mockRejectedValue(
      new ApiError(400, "目标父分类的祖先链包含自身，会形成环", "invalid_request", "req-1"),
    );
    render(<App />);
    await waitFor(() => expect(screen.getByRole("link", { name: "Rust" })).toBeTruthy());

    await selectOption("移动 技术", "Rust");
    // 移动必须带上当前版本（后端据此判定并发冲突）。
    await waitFor(() =>
      expect(categoryApi.update).toHaveBeenCalledWith("tech", {
        name: "技术",
        parent: "rust",
        expected_version: 1,
      }),
    );
    expect(await screen.findByText(/环/)).toBeTruthy();
  });

  it("删除保护展示 category_in_use 文案", async () => {
    vi.mocked(categoryApi.remove).mockRejectedValue(
      new ApiError(409, "分类仍被 2 篇文章引用、仍有 1 个子分类；先解除引用并移走子分类", "category_in_use", "req-2"),
    );
    render(<App />);
    await waitFor(() => expect(screen.getByRole("link", { name: "技术" })).toBeTruthy());
    fireEvent.click(within(screen.getByRole("link", { name: "技术" }).closest("tr")!).getByRole("button", { name: "删除" }));
    // 确认弹窗改由 antd 的 modal.confirm 渲染，必须点掉它才会发请求。
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));
    await waitFor(() => expect(categoryApi.remove).toHaveBeenCalledWith("tech", 1));
    expect(await screen.findByText(/子分类/)).toBeTruthy();
  });

  it("删除确认被取消时不发请求", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByRole("link", { name: "技术" })).toBeTruthy());
    fireEvent.click(within(screen.getByRole("link", { name: "技术" }).closest("tr")!).getByRole("button", { name: "删除" }));
    fireEvent.click(await screen.findByRole("button", { name: "取消" }));
    expect(categoryApi.remove).not.toHaveBeenCalled();
  });
});
