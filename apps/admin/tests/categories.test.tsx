// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError, categoryApi } from "../src/api";
import { navigate, paths } from "../src/router";
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

beforeEach(() => {
  vi.resetAllMocks();
  window.history.replaceState(null, "", paths.categories);
  vi.mocked(categoryApi.list).mockResolvedValue([rust, tech]);
});
afterEach(cleanup);

describe("分类管理屏", () => {
  it("树形列出目录并创建子分类", async () => {
    vi.mocked(categoryApi.create).mockResolvedValue(tech);
    render(<App />);
    await waitFor(() => expect(screen.getByRole("link", { name: "Rust" })).toBeTruthy());

    fireEvent.change(screen.getByLabelText("名称"), { target: { value: "生活" } });
    fireEvent.change(screen.getByLabelText("slug"), { target: { value: "life" } });
    fireEvent.change(screen.getByLabelText("父分类"), { target: { value: "tech" } });
    fireEvent.click(screen.getByRole("button", { name: "创建分类" }));

    await waitFor(() =>
      expect(categoryApi.create).toHaveBeenCalledWith({ name: "生活", slug: "life", parent: "tech" }),
    );
  });

  it("移动成环展示服务端错误文案", async () => {
    vi.mocked(categoryApi.update).mockRejectedValue(
      new ApiError(400, "目标父分类的祖先链包含自身，会形成环", "invalid_request", "req-1"),
    );
    window.confirm = vi.fn(() => true);
    render(<App />);
    await waitFor(() => expect(screen.getByRole("link", { name: "Rust" })).toBeTruthy());

    fireEvent.change(screen.getByLabelText("移动 技术"), { target: { value: "rust" } });
    await waitFor(() => expect(categoryApi.update).toHaveBeenCalled());
    expect(screen.getByText(/环/)).toBeTruthy();
  });

  it("删除保护展示 category_in_use 文案并重载", async () => {
    vi.mocked(categoryApi.remove).mockRejectedValue(
      new ApiError(409, "分类仍被 2 篇文章引用、仍有 1 个子分类；先解除引用并移走子分类", "category_in_use", "req-2"),
    );
    window.confirm = vi.fn(() => true);
    render(<App />);
    await waitFor(() => expect(screen.getByRole("link", { name: "技术" })).toBeTruthy());
    fireEvent.click(screen.getAllByRole("button", { name: "删除" })[1]);
    await waitFor(() => expect(categoryApi.remove).toHaveBeenCalledWith("tech", 1));
    expect(screen.getByText(/子分类/)).toBeTruthy();
  });
});
