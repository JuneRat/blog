import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { pagesApi } from "../src/api/pages";
import type { NavigationItem } from "../src/api/generated";
import { NavigationEditor } from "../src/components/NavigationEditor";
import { AdminProviders } from "../src/providers";
import { postResponse } from "./httpFixtures";

vi.mock("../src/api/pages", () => ({ pagesApi: { listPages: vi.fn() } }));
function Navigation({ canReadPages = true }) {
  const [value, onChange] = useState<NavigationItem[]>([{ label: "关于", page_slug: "", placement: "header" }]);
  return <AdminProviders><NavigationEditor {...{ value, onChange, canReadPages }} /></AdminProviders>;
}
beforeEach(() => { vi.mocked(pagesApi.listPages).mockReset().mockResolvedValue({ items: [], total: 0, page: 1, per_page: 50 }); });
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

it("连续输入保持焦点，并从服务端搜索不在首页的页面", async () => {
  vi.mocked(pagesApi.listPages).mockImplementation(async filter => ({
    items: filter?.q === "about" ? [postResponse({ slug: "about", title: "关于我们" })] : [],
    total: filter?.q === "about" ? 1 : 0, page: 1, per_page: 50,
  }));
  render(<Navigation />);
  const input = screen.getByRole("combobox", { name: "导航 1 目标页面" });
  act(() => input.focus());
  for (const value of ["a", "ab", "about"]) {
    fireEvent.change(input, { target: { value } });
    expect(document.activeElement).toBe(input);
    expect(screen.getByRole("combobox", { name: "导航 1 目标页面" })).toBe(input);
  }
  await waitFor(() => expect(pagesApi.listPages).toHaveBeenCalledWith({ page: 1, q: "about" }));
  expect(await screen.findByRole("option", { name: "关于我们 (/about)" })).toBeTruthy();
});

it("没有页面读取权限仍能手动编辑导航，且不发起页面查询", async () => {
  render(<Navigation canReadPages={false} />);
  const input = screen.getByRole("combobox", { name: "导航 1 目标页面" }) as HTMLInputElement;
  act(() => input.focus());
  fireEvent.change(input, { target: { value: "manual" } });
  await act(() => new Promise(resolve => setTimeout(resolve, 350)));
  expect(input.value).toBe("manual");
  expect(pagesApi.listPages).not.toHaveBeenCalled();
});

it("查询失败给出提示并保留手动输入", async () => {
  vi.mocked(pagesApi.listPages).mockRejectedValue(new Error("offline"));
  render(<Navigation />);
  const input = screen.getByRole("combobox", { name: "导航 1 目标页面" }) as HTMLInputElement;
  act(() => input.focus());
  fireEvent.change(input, { target: { value: "about" } });
  expect(await screen.findByRole("status")).toHaveProperty("textContent", "页面建议加载失败，仍可手动填写页面 slug。");
  expect(input.value).toBe("about");
});
