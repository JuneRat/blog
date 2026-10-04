import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AdminProviders, useColorScheme } from "../src/providers";

function Appearance() {
  const { scheme, isDark, setScheme } = useColorScheme();
  return <><output>{scheme}:{isDark ? "dark" : "light"}</output>
    <button onClick={() => setScheme("dark")}>深色</button>
    <button onClick={() => setScheme("auto")}>自动</button></>;
}
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

it("浏览器禁止访问本地存储时仍可加载和切换外观", () => {
  vi.spyOn(window, "localStorage", "get").mockImplementation(() => { throw new DOMException("Blocked", "SecurityError"); });
  render(<AdminProviders><Appearance /></AdminProviders>);
  expect(screen.getByText("auto:light")).toBeTruthy();
  fireEvent.click(screen.getByText("深色"));
  expect(screen.getByText("dark:dark")).toBeTruthy();
});

it("手动选择跨挂载保留，切回自动后继续响应系统变化", () => {
  let changed: (event: MediaQueryListEvent) => void = () => {};
  const original = window.matchMedia;
  vi.spyOn(window, "matchMedia").mockImplementation(query => ({
    ...original(query), matches: false,
    addEventListener: (_type: string, listener: EventListenerOrEventListenerObject) => { changed = listener as typeof changed; },
    removeEventListener: () => {},
  }));
  const mounted = render(<AdminProviders><Appearance /></AdminProviders>);
  fireEvent.click(screen.getByText("深色"));
  expect(localStorage.getItem("admin_color_scheme")).toBe("dark");
  mounted.unmount();
  render(<AdminProviders><Appearance /></AdminProviders>);
  expect(screen.getByText("dark:dark")).toBeTruthy();
  fireEvent.click(screen.getByText("自动"));
  expect(screen.getByText("auto:light")).toBeTruthy();
  act(() => changed({ matches: true } as MediaQueryListEvent));
  expect(screen.getByText("auto:dark")).toBeTruthy();
});
