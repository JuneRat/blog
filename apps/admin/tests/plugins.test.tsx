// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError } from "../src/api/client";
import type { PluginsView } from "../src/api/generated";
import { pluginsApi } from "../src/api/plugins";
import { paths } from "../src/router";

const permissions = vi.hoisted(() => ({ value: ["plugins.manage"] }));
vi.mock("../src/auth", () => ({ useAuth: () => ({ status: "authenticated", me: { permissions: permissions.value } }) }));
vi.mock("../src/api/plugins", () => ({ pluginsApi: { get: vi.fn(), save: vi.fn() } }));

const fixture: PluginsView = { version: 4, plugins: [{
  id: "notation", name: "示例扩展", description: "扩展描述", version: "1.0", available: true, enabled: false,
  hooks: ["content", "page_head"], config: { label: "初始值", compact: true, size: 12 }, config_fields: [
    { key: "label", label: "显示名称", description: "用于显示", default: "" },
    { key: "compact", label: "紧凑显示", description: "", default: false },
    { key: "size", label: "字号", description: "", default: 12 },
  ],
}] };
beforeEach(() => {
  vi.resetAllMocks(); permissions.value = ["plugins.manage"];
  window.history.replaceState(null, "", paths.plugins);
  vi.mocked(pluginsApi.get).mockResolvedValue(structuredClone(fixture));
});
afterEach(cleanup);

it("has an independent plugin entry and an honest empty catalog", async () => {
  vi.mocked(pluginsApi.get).mockResolvedValue({ version: 0, plugins: [] });
  render(<App />);
  await screen.findByText("暂无可用插件");
  expect(screen.getByRole("menuitem", { name: "插件管理" })).toBeTruthy();
  expect(screen.queryByRole("switch")).toBeNull();
});

it("does not fetch or expose controls to a settings manager without plugin permission", async () => {
  permissions.value = ["settings.manage"];
  render(<App />);
  await screen.findByText("当前账号没有管理插件的权限。");
  expect(pluginsApi.get).not.toHaveBeenCalled();
});

it("toggles using the latest version returned by the previous save", async () => {
  vi.mocked(pluginsApi.save).mockImplementation(async (_id, input) => ({
    version: input.expected_version + 1, plugins: [{ ...fixture.plugins[0], enabled: input.enabled, config: input.config }],
  }));
  render(<App />);
  fireEvent.click(await screen.findByRole("switch", { name: "启用 示例扩展" }));
  await waitFor(() => expect(screen.getByRole("switch", { name: "启用 示例扩展" }).getAttribute("aria-checked")).toBe("true"));
  expect(pluginsApi.save).toHaveBeenLastCalledWith("notation", { enabled: true, config: fixture.plugins[0].config, expected_version: 4 });
  fireEvent.click(screen.getByRole("switch", { name: "启用 示例扩展" }));
  await waitFor(() => expect(pluginsApi.save).toHaveBeenLastCalledWith("notation", { enabled: false, config: fixture.plugins[0].config, expected_version: 5 }));
});

it("retains typed configuration input after a conflict and never retries the write", async () => {
  vi.mocked(pluginsApi.save).mockRejectedValue(new ApiError(409, "conflict", "version_conflict"));
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "配置 示例扩展" }));
  fireEvent.change(screen.getByLabelText("显示名称"), { target: { value: "我的输入" } });
  fireEvent.click(screen.getByRole("switch", { name: "紧凑显示" }));
  fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
  await screen.findByText(/当前输入已保留/);
  expect((screen.getByLabelText("显示名称") as HTMLInputElement).value).toBe("我的输入");
  expect(pluginsApi.save).toHaveBeenCalledExactlyOnceWith("notation", { enabled: false, config: { label: "我的输入", compact: false, size: 12 }, expected_version: 4 });
});

it("shows removed plugins and permits disabling them without inventing configuration controls", async () => {
  vi.mocked(pluginsApi.get).mockResolvedValue({ version: 2, plugins: [{ ...fixture.plugins[0], available: false, enabled: true, config_fields: [] }] });
  vi.mocked(pluginsApi.save).mockResolvedValue({ version: 3, plugins: [{ ...fixture.plugins[0], available: false, enabled: false, config_fields: [] }] });
  render(<App />);
  await screen.findByText("插件不可用");
  expect(screen.queryByRole("button", { name: "配置 示例扩展" })).toBeNull();
  fireEvent.click(screen.getByRole("switch", { name: "启用 示例扩展" }));
  await waitFor(() => expect(pluginsApi.save).toHaveBeenCalled());
});
