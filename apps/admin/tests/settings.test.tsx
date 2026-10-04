// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError } from "../src/api/client";
import { identityApi } from "../src/api/identity";
import { commentsApi } from "../src/api/comments";
import { mediaApi } from "../src/api/media";
import { settingsApi, themeSettingsApi, retentionApi } from "../src/api/settings";
import { paths } from "../src/router";
import type { SiteSettings } from "../src/types";

const updateTimeZone = vi.hoisted(() => vi.fn());

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    updateTimeZone,
    status: "authenticated",
    me: { permissions: ["settings.manage", "media.read", "media.upload"] },
  }),
}));

vi.mock("../src/api/identity", async (load) => {
  const original = await load<typeof import("../src/api/identity")>();
  return { ...original, identityApi: { ...original.identityApi, accessSettings: vi.fn(), saveAccessSettings: vi.fn() } };
});
vi.mock("../src/api/comments", async (load) => {
  const original = await load<typeof import("../src/api/comments")>();
  return { ...original, commentsApi: { ...original.commentsApi, policy: vi.fn(), savePolicy: vi.fn() } };
});
vi.mock("../src/api/settings", async (load) => {
  const original = await load<typeof import("../src/api/settings")>();
  return { ...original, settingsApi: { get: vi.fn(), save: vi.fn() }, themeSettingsApi: { get: vi.fn(), save: vi.fn(), install: vi.fn(), upgrade: vi.fn(), previous: vi.fn(), rollback: vi.fn(), preview: vi.fn(), validatePackage: vi.fn(), validateInstalled: vi.fn(), uninstall: vi.fn(), getConfig: vi.fn(), saveConfig: vi.fn() }, retentionApi: { get: vi.fn(), save: vi.fn() } };
});
vi.mock("../src/api/media", async (load) => {
  const original = await load<typeof import("../src/api/media")>();
  return { ...original, mediaApi: { ...original.mediaApi, list: vi.fn(), upload: vi.fn() } };
});

const fallbackView: SiteSettings = {
  home_page_size: 12,
  navigation: [],
  time_zone: "UTC",
  time_zones: ["UTC", "Asia/Shanghai", "America/New_York"],
  title: "默认站点",
  description: "回退描述",
  logo_media_id: null,
  logo_url: null,
  source: "fallback",
  version: 0,
};

const logoAsset = {
  id: "logo-1",
  original_name: "logo.png",
  mime: "image/png",
  byte_size: 10,
  width: 32,
  height: 32,
  deleted_at: null,
  version: 2,
  created_at: "2026-01-01",
  owner_id: "u1",
  owner_display: "管理员",
  url: "/media/logo-1",
  reference_count: 0,
};

beforeEach(() => {
  vi.resetAllMocks();
  window.history.replaceState(null, "", paths.settings);
  vi.mocked(settingsApi.get).mockResolvedValue(fallbackView);
  vi.mocked(identityApi.accessSettings).mockResolvedValue({ registration_enabled: false, guest_comments_enabled: false, version: 0 });
  vi.mocked(commentsApi.policy).mockResolvedValue({ enabled: true, moderation: 'all', version: 4 });
  vi.mocked(retentionApi.get).mockResolvedValue({ comment_ip_days: 180, comment_version: 0, audit_days: 180, audit_version: 0 });
  vi.mocked(themeSettingsApi.get).mockResolvedValue({ slug: "default", effective_slug: "default", fallback_slug: "default", source: "fallback", version: 0, available: [{ slug: "default", name: "Default", release: "default-release" }, { slug: "paper", name: "Paper", release: "paper-release", id: "paper-id", config_version: 1, config_schema_version: 1 }] });
});
afterEach(cleanup);

describe("站点设置与主题管理", () => {
  it("先验证 ZIP 再安装，验证不激活，新主题立即可选且保留待激活选择", async () => {
    const report = { slug: "custom", name: "Custom", release: "a".repeat(64), template_count: 6, asset_count: 1 };
    vi.mocked(themeSettingsApi.validatePackage).mockResolvedValue(report);
    vi.mocked(themeSettingsApi.install).mockResolvedValue(report);
    const initial = await themeSettingsApi.get();
    vi.mocked(themeSettingsApi.get).mockResolvedValueOnce(initial).mockResolvedValue({ ...initial, available: [...initial.available, { slug: "custom", name: "Custom", release: "custom-release" }] });
    render(<App />);
    fireEvent.click(await screen.findByRole("menuitem", { name: "主题管理" }));
    const paper = await screen.findByRole("button", { name: "选择主题 Paper" });
    fireEvent.click(paper);
    const file = new File(["zip"], "custom.zip", { type: "application/zip" });
    fireEvent.change(screen.getByLabelText("主题 ZIP 包"), { target: { files: [file] } });
    fireEvent.click(screen.getByRole("button", { name: "验证主题包" }));
    await screen.findByText(/验证通过，可安装/);
    expect(themeSettingsApi.validatePackage).toHaveBeenCalledWith(file);
    expect(themeSettingsApi.install).not.toHaveBeenCalled();
    expect(themeSettingsApi.save).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "安装主题" }));
    await screen.findByRole("button", { name: "选择主题 Custom" });
    expect(themeSettingsApi.install).toHaveBeenCalledWith(file);
    expect(paper.getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("button", { name: "安装主题" }).hasAttribute("disabled")).toBe(true);
  });

  it("同名包升级携带配置版本并保留配置，冲突时保留文件", async () => {
    const report = { slug: "paper", name: "Paper", release: "new-release", template_count: 6, asset_count: 1 };
    vi.mocked(themeSettingsApi.validatePackage).mockResolvedValue(report);
    vi.mocked(themeSettingsApi.upgrade).mockRejectedValue(new ApiError(409, "版本冲突", "version_conflict"));
    render(<App />);
    fireEvent.click(await screen.findByRole("menuitem", { name: "主题管理" }));
    const file = new File(["zip"], "paper.zip");
    fireEvent.change(await screen.findByLabelText("主题 ZIP 包"), { target: { files: [file] } });
    fireEvent.click(screen.getByRole("button", { name: "验证主题包" }));
    fireEvent.click(await screen.findByRole("button", { name: "保留配置升级" }));
    expect(themeSettingsApi.upgrade).not.toHaveBeenCalled();
    fireEvent.click((await screen.findAllByRole("button", { name: "保留配置升级" }))[1]);
    await waitFor(() => expect(themeSettingsApi.upgrade).toHaveBeenCalledWith("paper", file, { id: "paper-id", expected_version: 1, expected_release: "paper-release" }));
    await screen.findByText(/版本冲突/);
    expect(screen.getByText("paper.zip")).toBeTruthy();
    expect(themeSettingsApi.install).not.toHaveBeenCalled();
    expect(themeSettingsApi.save).not.toHaveBeenCalled();
  });

  it("预览使用隔离 iframe 且不激活，回退先展示上一版再确认", async () => {
    vi.mocked(themeSettingsApi.preview).mockResolvedValue({ html: '<html><head><script>bad()</script><meta http-equiv="refresh" content="0;url=https://outside.test"></head><body><p>preview</p></body></html>' });
    vi.mocked(themeSettingsApi.previous).mockResolvedValue({ previous: { slug: "paper", name: "Paper", release: "old-release", template_count: 6, asset_count: 1 } });
    vi.mocked(themeSettingsApi.rollback).mockResolvedValue({ slug: "paper", name: "Paper", release: "old-release", template_count: 6, asset_count: 1 });
    render(<App />);
    fireEvent.click(await screen.findByRole("menuitem", { name: "主题管理" }));
    fireEvent.click(await screen.findByRole("button", { name: "预览主题 Paper" }));
    const frame = await screen.findByTitle("主题首页预览");
    expect(frame.getAttribute("sandbox")).toBe("");
    expect(frame.getAttribute("srcdoc")).toContain("default-src 'none'");
    expect(frame.getAttribute("srcdoc")).not.toContain("bad()");
    expect(frame.getAttribute("srcdoc")).not.toContain("outside.test");
    expect(themeSettingsApi.save).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "关闭" }));
    fireEvent.click(screen.getByRole("button", { name: "回退主题 Paper" }));
    await screen.findByText(/old-release/);
    expect(themeSettingsApi.rollback).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "回退上一版" }));
    await waitFor(() => expect(themeSettingsApi.rollback).toHaveBeenCalledWith("paper", { id: "paper-id", expected_version: 1, expected_release: "paper-release" }));
  });

  it("验证已安装快照不改变选择，默认和活动主题禁止卸载", async () => {
    vi.mocked(themeSettingsApi.get).mockResolvedValue({ slug: "paper", effective_slug: "paper", fallback_slug: "default", source: "database", version: 4, available: [{ slug: "default", name: "Default", release: "default-release" }, { slug: "paper", name: "Paper", release: "paper-release", id: "paper-id", config_version: 1, config_schema_version: 1 }] });
    vi.mocked(themeSettingsApi.validateInstalled).mockResolvedValue({ slug: "default", name: "Default", release: "b".repeat(64), template_count: 7, asset_count: 1 });
    render(<App />);
    fireEvent.click(await screen.findByRole("menuitem", { name: "主题管理" }));
    const verify = await screen.findByRole("button", { name: "验证主题 Default" });
    expect(screen.getByRole("button", { name: "卸载主题 Default" }).hasAttribute("disabled")).toBe(true);
    expect(screen.getByRole("button", { name: "卸载主题 Paper" }).hasAttribute("disabled")).toBe(true);
    fireEvent.click(verify);
    await screen.findByText(/已安装快照验证通过/);
    expect(themeSettingsApi.validateInstalled).toHaveBeenCalledWith("default");
    expect(screen.getByRole("button", { name: "选择主题 Paper" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("卸载前确认并携带激活版本，卸载待选主题后恢复当前生效选择", async () => {
    const initial = await themeSettingsApi.get();
    vi.mocked(themeSettingsApi.uninstall).mockResolvedValue({ ...initial, available: [initial.available[0]] });
    render(<App />);
    fireEvent.click(await screen.findByRole("menuitem", { name: "主题管理" }));
    fireEvent.click(await screen.findByRole("button", { name: "选择主题 Paper" }));
    fireEvent.click(screen.getByRole("button", { name: "卸载主题 Paper" }));
    expect(themeSettingsApi.uninstall).not.toHaveBeenCalled();
    fireEvent.click(await screen.findByRole("button", { name: "卸载" }));
    await screen.findByText(/主题「paper」已卸载/);
    expect(themeSettingsApi.uninstall).toHaveBeenCalledWith("paper", 0, "paper-release", { id: "paper-id", version: 1, config_schema_version: 1 });
    expect(screen.queryByRole("button", { name: "选择主题 Paper" })).toBeNull();
    expect(screen.getByRole("button", { name: "选择主题 Default" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("损坏包显示验证原因且不安装，换包会清除上次验证状态", async () => {
    vi.mocked(themeSettingsApi.validatePackage).mockRejectedValue(new ApiError(400, "主题验证失败：empty/index.html", "invalid", "theme-1"));
    render(<App />);
    fireEvent.click(await screen.findByRole("menuitem", { name: "主题管理" }));
    const input = await screen.findByLabelText("主题 ZIP 包");
    fireEvent.change(input, { target: { files: [new File(["zip"], "broken.zip")] } });
    fireEvent.click(screen.getByRole("button", { name: "验证主题包" }));
    await screen.findByText(/主题验证失败：empty\/index.html/);
    expect(themeSettingsApi.install).not.toHaveBeenCalled();
    fireEvent.change(input, { target: { files: [new File([], "empty.zip")] } });
    await screen.findByText(/请选择非空且不超过 10 MiB/);
    expect(screen.getByRole("button", { name: "安装主题" }).hasAttribute("disabled")).toBe(true);
  });

  it("卸载版本冲突重取最新视图，不自动重试已经生效的主题", async () => {
    const initial = await themeSettingsApi.get();
    vi.mocked(themeSettingsApi.get).mockResolvedValueOnce(initial).mockResolvedValue({ ...initial, slug: "paper", effective_slug: "paper", source: "database", version: 1 });
    vi.mocked(themeSettingsApi.uninstall).mockRejectedValue(new ApiError(409, "版本冲突", "version_conflict", "theme-2"));
    render(<App />);
    fireEvent.click(await screen.findByRole("menuitem", { name: "主题管理" }));
    fireEvent.click(await screen.findByRole("button", { name: "卸载主题 Paper" }));
    fireEvent.click(await screen.findByRole("button", { name: "卸载" }));
    await screen.findByText(/当前主题：Paper/);
    expect(themeSettingsApi.uninstall).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "卸载主题 Paper" }).hasAttribute("disabled")).toBe(true);
    expect(themeSettingsApi.save).not.toHaveBeenCalled();
  });

  it("站点设置保留三组配置，主题、插件和任务管理使用独立入口", async () => {
    render(<App />);
    await screen.findByLabelText("站点时区");
    expect(screen.getAllByRole("tab").map(tab => tab.textContent)).toEqual(["常规设置", "账号与评论", "数据保留"]);
    expect(screen.getByRole("menuitem", { name: "主题管理" })).toBeTruthy();
    expect(screen.getByRole("menuitem", { name: "插件管理" })).toBeTruthy();
    expect(screen.queryByRole("tab", { name: "内容维护" })).toBeNull();
    expect(screen.getByRole("menuitem", { name: "任务管理" })).toBeTruthy();
  });
  it("评论审核策略和全站开关立即保存，并沿用最新设置版本", async () => {
    vi.mocked(commentsApi.savePolicy).mockImplementation(async policy => {
      const updated = { ...policy, moderation: policy.moderation ?? 'first_comment' as const, version: policy.version + 1 };
      vi.mocked(commentsApi.policy).mockResolvedValue(updated);
      return updated;
    });
    render(<App />);
    fireEvent.click(await screen.findByRole('tab', { name: '账号与评论' }));
    const selector = await screen.findByRole('combobox', { name: '审核策略' });
    await waitFor(() => expect(selector.hasAttribute('disabled')).toBe(false));
    fireEvent.mouseDown(selector);
    fireEvent.click(await screen.findByTitle('首次评论审核'));
    await waitFor(() => expect(commentsApi.savePolicy).toHaveBeenCalledWith({ enabled: true, moderation: 'first_comment', version: 4 }, undefined));
    const toggle = screen.getByRole('switch', { name: '允许全站评论' });
    await waitFor(() => expect((toggle as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(toggle);
    await waitFor(() => expect(commentsApi.savePolicy).toHaveBeenLastCalledWith({ enabled: false, version: 5 }, undefined));
    await screen.findByText(/账号已有人工审核通过且仍保留为通过状态/);
    await waitFor(() => expect(toggle.getAttribute('aria-checked')).toBe('false'));
  });
  it("保存时区后立即更新后台显示，并保留等待期间的新选择", async () => {
    let resolveSave!: (view: SiteSettings) => void;
    vi.mocked(settingsApi.save).mockImplementation(() => new Promise((resolve) => { resolveSave = resolve; }));
    render(<App />);
    const zone = await screen.findByLabelText("站点时区");
    fireEvent.mouseDown(zone);
    fireEvent.click(await screen.findByTitle("Asia/Shanghai"));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(settingsApi.save).toHaveBeenCalledWith(expect.objectContaining({
      navigation: [],
      title: fallbackView.title, description: fallbackView.description, logo_media_id: null,
      time_zone: "Asia/Shanghai", expected_version: 0,
    })));
    fireEvent.mouseDown(zone);
    fireEvent.click(await screen.findByTitle("America/New_York"));
    await act(async () => resolveSave({ ...fallbackView, time_zone: "Asia/Shanghai", source: "database", version: 1 }));
    await screen.findByText(/保存期间的新输入尚未提交/);
    expect(updateTimeZone).toHaveBeenCalledWith("Asia/Shanghai");
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(settingsApi.save).toHaveBeenLastCalledWith(expect.objectContaining({
      time_zone: "America/New_York", expected_version: 1,
    })));
    await act(async () => resolveSave({ ...fallbackView, time_zone: "America/New_York", source: "database", version: 2 }));
    await screen.findByText(/已保存（v2）/);
    expect(updateTimeZone).toHaveBeenLastCalledWith("America/New_York");
  });
  it("保留期保存携带两组版本，冲突保留输入并阻止重复提交", async () => {
    vi.mocked(retentionApi.save).mockRejectedValue(new ApiError(409, "版本冲突", "version_conflict"));
    render(<App />);
    fireEvent.click(await screen.findByRole("tab", { name: "数据保留" }));
    const ip = await screen.findByLabelText("评论 IP 保留天数");
    fireEvent.change(ip, { target: { value: "60" } });
    fireEvent.blur(ip);
    fireEvent.click(screen.getByRole("button", { name: "保存保留期" }));
    await waitFor(() => expect(retentionApi.save).toHaveBeenCalledWith(expect.objectContaining({ comment_ip_days: 60, comment_version: 0, audit_days: 180, audit_version: 0 })));
    await screen.findByText(/你的输入已保留，请重新加载后再编辑/);
    expect((ip as HTMLInputElement).value).toBe("60");
    expect((screen.getByRole("button", { name: "保存保留期" }) as HTMLButtonElement).disabled).toBe(true);
    vi.mocked(retentionApi.get).mockResolvedValue({ comment_ip_days: 90, comment_version: 2, audit_days: 365, audit_version: 3 });
    fireEvent.click(screen.getByRole("button", { name: "重新加载保留期并放弃修改" }));
    await waitFor(() => expect((ip as HTMLInputElement).value).toBe("90"));
    vi.mocked(retentionApi.save).mockResolvedValue({ comment_ip_days: 91, comment_version: 3, audit_days: 365, audit_version: 3 });
    fireEvent.change(ip, { target: { value: "91" } });
    fireEvent.blur(ip);
    fireEvent.click(screen.getByRole("button", { name: "保存保留期" }));
    await waitFor(() => expect(retentionApi.save).toHaveBeenLastCalledWith(expect.objectContaining({ comment_ip_days: 91, comment_version: 2, audit_days: 365, audit_version: 3 })));
    await screen.findByText("保留期已保存，下次维护时生效。");
  });
  it("切换主题携带版本并显示即时生效", async () => {
    vi.mocked(themeSettingsApi.save).mockResolvedValue({ slug: "paper", effective_slug: "paper", fallback_slug: "default", source: "database", version: 1, available: [{ slug: "default", name: "Default", release: "default-release" }, { slug: "paper", name: "Paper", release: "paper-release", id: "paper-id", config_version: 1, config_schema_version: 1 }] });
    render(<App />);
    fireEvent.click(await screen.findByRole("menuitem", { name: "主题管理" }));
    // 主题下拉是 antd Select，不是原生控件：fireEvent.change 改不动它的值。
    // 先等主题加载完成（否则展开的是空列表），再 mouseDown 展开、点选项文案。
    await screen.findByText(/当前主题：Default/);
    const select = await screen.findByLabelText("选择主题");
    fireEvent.mouseDown(select);
    fireEvent.click(await screen.findByTitle("Paper"));
    fireEvent.click(screen.getByRole("button", { name: "激活主题" }));
    await waitFor(() => expect(themeSettingsApi.save).toHaveBeenCalledWith("paper", 0));
    await waitFor(() => expect(screen.getByText(/主题已切换为「Paper」/)).toBeTruthy());
  });

  it("点击主题卡片可选择主题并触发切换保存", async () => {
    vi.mocked(themeSettingsApi.save).mockResolvedValue({
      slug: "paper",
      effective_slug: "paper", fallback_slug: "default",
      source: "database",
      version: 1,
      available: [
        { slug: "default", name: "Default", release: "default-release" },
        { slug: "paper", name: "Paper", release: "paper-release", id: "paper-id", config_version: 1, config_schema_version: 1 },
      ],
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("menuitem", { name: "主题管理" }));
    await screen.findByText(/当前主题：Default/);
    fireEvent.click(screen.getByText("Paper"));
    fireEvent.click(screen.getByRole("button", { name: "激活主题" }));
    await waitFor(() => expect(themeSettingsApi.save).toHaveBeenCalledWith("paper", 0));
    await waitFor(() => expect(screen.getByText(/主题已切换为「Paper」/)).toBeTruthy());
  });

  it("键盘选择待生效主题后拦截离开，恢复原选择或保存后清除保护", async () => {
    vi.mocked(themeSettingsApi.save).mockResolvedValue({ slug: "paper", effective_slug: "paper", fallback_slug: "default", source: "database", version: 1,
      available: [{ slug: "default", name: "Default", release: "default-release" }, { slug: "paper", name: "Paper", release: "paper-release", id: "paper-id", config_version: 1, config_schema_version: 1 }] });
    render(<App />);
    fireEvent.click(await screen.findByRole("menuitem", { name: "主题管理" }));
    const paper = await screen.findByRole("button", { name: "选择主题 Paper" });
    fireEvent.keyDown(paper, { key: "Enter" });
    expect(paper.getAttribute("aria-pressed")).toBe("true");
    const departure = new Event("beforeunload", { cancelable: true });
    act(() => { window.dispatchEvent(departure); });
    expect(departure.defaultPrevented).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "个人资料" }));
    await screen.findByRole("dialog", { name: "有未保存的修改" });
    fireEvent.click(screen.getByRole("button", { name: "留在此页" }));
    expect(window.location.pathname).toBe(paths.themes);
    fireEvent.keyDown(screen.getByRole("button", { name: "选择主题 Default" }), { key: " " });
    const reverted = new Event("beforeunload", { cancelable: true });
    act(() => { window.dispatchEvent(reverted); });
    expect(reverted.defaultPrevented).toBe(false);
    fireEvent.keyDown(paper, { key: "Enter" });
    fireEvent.click(screen.getByRole("button", { name: "激活主题" }));
    await screen.findByText(/主题已切换为「Paper」/);
    const saved = new Event("beforeunload", { cancelable: true });
    act(() => { window.dispatchEvent(saved); });
    expect(saved.defaultPrevented).toBe(false);
  });

  it("已保存主题缺失时提示默认主题并允许修复", async () => {
    vi.mocked(themeSettingsApi.get).mockResolvedValue({ slug: "removed", effective_slug: "default", fallback_slug: "default", source: "database", version: 3, available: [{ slug: "default", name: "Default", release: "default-release" }, { slug: "paper", name: "Paper", release: "paper-release", id: "paper-id", config_version: 1, config_schema_version: 1 }] });
    vi.mocked(themeSettingsApi.save).mockResolvedValue({ slug: "default", effective_slug: "default", fallback_slug: "default", source: "database", version: 4, available: [{ slug: "default", name: "Default", release: "default-release" }, { slug: "paper", name: "Paper", release: "paper-release", id: "paper-id", config_version: 1, config_schema_version: 1 }] });
    render(<App />);
    fireEvent.click(await screen.findByRole("menuitem", { name: "主题管理" }));
    expect(await screen.findByText(/已保存的主题「removed」当前未安装/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "激活主题" }));
    await waitFor(() => expect(themeSettingsApi.save).toHaveBeenCalledWith("default", 3));
  });
  it("加载生效值并标注来源为内置默认值", async () => {
    render(<App />);
    const title = (await screen.findByLabelText("站点标题")) as HTMLInputElement;
    expect(title.value).toBe("默认站点");
    expect(
      screen.getByText(/内置默认值（数据库尚未配置；保存后由数据库接管）/),
    ).toBeTruthy();
  });

  it("保存携带当前版本并展示数据库来源提示", async () => {
    vi.mocked(settingsApi.save).mockResolvedValue({
      ...fallbackView,
      title: "数据库站点",
      description: "新描述",
      source: "database",
      version: 1,
    });
    render(<App />);

    fireEvent.change(await screen.findByLabelText("站点标题"), {
      target: { value: " 数据库站点 " },
    });
    fireEvent.change(screen.getByLabelText("站点描述"), { target: { value: "新描述" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() =>
      expect(settingsApi.save).toHaveBeenCalledWith(expect.objectContaining({
      navigation: [],
        time_zone: "UTC",
        title: "数据库站点",
        description: "新描述",
        logo_media_id: null,
        expected_version: 0,
      })),
    );
    await waitFor(() => expect(screen.getByText(/已保存（v1）/)).toBeTruthy());
    expect(screen.getByText(/当前生效来源：数据库（v1）/)).toBeTruthy();
  });

  it("空标题不提交", async () => {
    render(<App />);
    const title = await screen.findByLabelText("站点标题");
    fireEvent.change(title, { target: { value: "   " } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    // antd Form 的 onFinish 在异步校验之后才触发，错误文案不是点击后同步出现。
    await waitFor(() => expect(screen.getByText("站点标题不能为空。")).toBeTruthy());
    expect(settingsApi.save).not.toHaveBeenCalled();
  });

  it("版本冲突进入冲突流程：仍然覆盖按服务器版本重提", async () => {
    vi.mocked(settingsApi.save)
      .mockRejectedValueOnce(
        new ApiError(409, "版本冲突：内容已被并发修改", "version_conflict", "req-1"),
      )
      .mockResolvedValueOnce({
        ...fallbackView,
        title: "我的标题",
        description: "描述",
        source: "database",
        version: 3,
      });
    // 冲突后的重载返回服务器最新视图（v2，标题已被别处修改）。
    vi.mocked(settingsApi.get)
      .mockResolvedValueOnce({
        ...fallbackView,
        title: "第一版",
        description: "描述",
        source: "database",
        version: 1,
      })
      .mockResolvedValueOnce({
        ...fallbackView,
        title: "别处的修改",
        description: "描述",
        source: "database",
        version: 2,
      })
      // 保存成功会让站点设置查询失效并在后台重取（这里补上兜底返回值，
      // 避免那次后台读取拿到 undefined）。
      .mockResolvedValue({
        ...fallbackView,
        title: "我的标题",
        description: "描述",
        source: "database",
        version: 3,
      });
    render(<App />);

    fireEvent.change(await screen.findByLabelText("站点标题"), {
      target: { value: "我的标题" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() =>
      expect(screen.getByText(/设置已在别处被修改（服务器当前：v2「别处的修改」）/)).toBeTruthy(),
    );
    // 本地输入保留。
    expect((screen.getByLabelText("站点标题") as HTMLInputElement).value).toBe("我的标题");

    fireEvent.click(screen.getByRole("button", { name: "仍然覆盖" }));
    // window.confirm 换成 antd modal.confirm：确认动作挪进弹窗的默认「确定」。
    fireEvent.click(await screen.findByRole("button", { name: "确定" }));
    await waitFor(() =>
      expect(settingsApi.save).toHaveBeenLastCalledWith(expect.objectContaining({
      navigation: [],
        time_zone: "UTC",
        logo_media_id: null,
        title: "我的标题",
        description: "描述",
        expected_version: 2,
      })),
    );
    await waitFor(() => expect(screen.getByText(/已保存（v3）/)).toBeTruthy());
  });

  it("冲突流程：重新加载丢弃本地改动", async () => {
    vi.mocked(settingsApi.save).mockRejectedValue(
      new ApiError(409, "版本冲突：内容已被并发修改", "version_conflict", "req-2"),
    );
    vi.mocked(settingsApi.get)
      .mockResolvedValueOnce({ ...fallbackView, source: "database", version: 1 })
      .mockResolvedValueOnce({
        ...fallbackView,
        title: "服务器标题",
        description: "服务器描述",
        source: "database",
        version: 2,
      });
    render(<App />);

    fireEvent.change(await screen.findByLabelText("站点标题"), {
      target: { value: "本地输入" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "重新加载" })).toBeTruthy(),
    );

    vi.mocked(settingsApi.save).mockClear();
    fireEvent.click(screen.getByRole("button", { name: "重新加载" }));
    await waitFor(() =>
      expect((screen.getByLabelText("站点标题") as HTMLInputElement).value).toBe("服务器标题"),
    );
    expect(screen.getByText(/已重新加载服务器当前值/)).toBeTruthy();
    expect(settingsApi.save).not.toHaveBeenCalled();
  });

  it("无权限时展示服务端 403 文案", async () => {
    vi.mocked(settingsApi.get).mockRejectedValue(
      new ApiError(403, "无权执行该操作", "forbidden", "req-3"),
    );
    render(<App />);
    await waitFor(() =>
      expect(screen.getByText(/没有权限：无权执行该操作（错误编号 req-3）/)).toBeTruthy(),
    );
  });

  it("保存响应不覆盖等待期间的新输入，并提示尚未提交", async () => {
    let resolveSave!: (view: SiteSettings) => void;
    vi.mocked(settingsApi.save).mockImplementation(
      () =>
        new Promise<SiteSettings>((resolve) => {
          resolveSave = resolve;
        }),
    );
    render(<App />);

    const title = (await screen.findByLabelText("站点标题")) as HTMLInputElement;
    fireEvent.change(title, { target: { value: "提交时的标题" } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(settingsApi.save).toHaveBeenCalled());

    // 请求未回来之前继续编辑：这段输入不能被响应覆盖。
    fireEvent.change(title, { target: { value: "等待期间的新输入" } });
    await act(async () => {
      resolveSave({
        ...fallbackView,
        title: "提交时的标题",
        description: "回退描述",
        source: "database",
        version: 1,
      });
    });

    await waitFor(() => expect(screen.getByText(/已保存（v1）/)).toBeTruthy());
    expect(title.value).toBe("等待期间的新输入");
    // 不能笼统显示「已保存」：新输入确实还没提交。
    expect(screen.getByText(/保存期间的新输入尚未提交，请再次保存/)).toBeTruthy();
    // 服务器版本已生效，再次保存基于 v1。
    expect(screen.getByText(/当前生效来源：数据库（v1）/)).toBeTruthy();
  });

  it("未被继续编辑的字段仍采用服务端规范化值（trim）", async () => {
    vi.mocked(settingsApi.save).mockResolvedValue({
      ...fallbackView,
      title: "去空白标题",
      description: "描述",
      source: "database",
      version: 1,
    });
    render(<App />);

    const title = (await screen.findByLabelText("站点标题")) as HTMLInputElement;
    fireEvent.change(title, { target: { value: "  去空白标题  " } });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(screen.getByText(/已保存（v1）/)).toBeTruthy());
    expect(title.value).toBe("去空白标题");
    expect(screen.queryByText(/尚未提交/)).toBeNull();
  });

  it("选择站点 logo 后保存：整组 PUT 携带 logo_media_id", async () => {
    vi.mocked(settingsApi.get).mockResolvedValue({
      ...fallbackView,
      source: "database",
      version: 1,
    });
    vi.mocked(mediaApi.list).mockResolvedValue({
      items: [logoAsset],
      total: 1,
      page: 1,
      per_page: 24,
    });
    vi.mocked(settingsApi.save).mockResolvedValue({
      ...fallbackView,
      source: "database",
      version: 2,
      logo_media_id: "logo-1",
      logo_url: "/media/logo-1",
    });

    render(<App />);
    // 站点 logo 的未设置态按钮（与文章/系列封面同一个选择器）。
    fireEvent.click(await screen.findByRole("button", { name: "选择封面" }));
    fireEvent.click(await screen.findByRole("button", { name: "选择" }));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(settingsApi.save).toHaveBeenCalled());
    expect(vi.mocked(settingsApi.save).mock.calls[0][0]).toMatchObject({
      logo_media_id: "logo-1",
      expected_version: 1,
    });
  });

  it("移除站点 logo 后保存：提交 logo_media_id: null（与「不改 logo」区分开）", async () => {
    vi.mocked(settingsApi.get).mockResolvedValue({
      ...fallbackView,
      source: "database",
      version: 3,
      logo_media_id: "logo-1",
      logo_url: "/media/logo-1",
    });
    vi.mocked(settingsApi.save).mockResolvedValue({
      ...fallbackView,
      source: "database",
      version: 4,
    });

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "移除封面" }));
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(settingsApi.save).toHaveBeenCalled());
    expect(vi.mocked(settingsApi.save).mock.calls[0][0]).toMatchObject({
      logo_media_id: null,
      expected_version: 3,
    });
  });
});


it("首页数量和页面导航顺序随站点设置保存", async () => {
  const navigation: SiteSettings["navigation"] = [
    { label: "关于", page_slug: "about", placement: "header" },
    { label: "联系", page_slug: "contact", placement: "footer" },
  ];
  const homePageSize = 8;
  vi.mocked(settingsApi.get).mockResolvedValue({ ...fallbackView, navigation });
  vi.mocked(settingsApi.save).mockResolvedValue({ ...fallbackView, home_page_size: homePageSize, navigation: [...navigation].reverse(), source: "database", version: 1 });
  render(<App />);
  fireEvent.change(await screen.findByLabelText("首页每页文章数"), { target: { value: String(homePageSize) } });
  fireEvent.click(screen.getByRole("button", { name: "上移导航 2" }));
  fireEvent.click(screen.getByRole("button", { name: "保存" }));
  await waitFor(() => expect(settingsApi.save).toHaveBeenCalledWith(expect.objectContaining({
    home_page_size: homePageSize, navigation: [...navigation].reverse(),
  })));
});

const themeConfigFixture = {
  id: "paper-record", slug: "paper", release: "paper-release", config_schema_version: 1, version: 3,
  config: { title: "默认页脚", show: true, count: 2, image: null }, overrides: {},
  fields: [
    { key: "title", type: "textarea" as const, label: "页脚文字", description: "可留空", group: "文字", default: "默认页脚", min_length: 0, max_length: 20, min: null, max: null, options: [] },
    { key: "show", type: "boolean" as const, label: "显示描述", description: "", group: "外观", default: true, min_length: null, max_length: null, min: null, max: null, options: [] },
    { key: "count", type: "integer" as const, label: "列数", description: "", group: "外观", default: 2, min_length: null, max_length: null, min: 1, max: 4, options: [] },
    { key: "image", type: "media" as const, label: "主题图片", description: "", group: "外观", default: null, min_length: null, max_length: null, min: null, max: null, options: [] },
  ],
};

it("按非活动主题声明生成表单，保存覆盖值并保留显式空字符串和 false", async () => {
  vi.mocked(themeSettingsApi.getConfig).mockResolvedValue(structuredClone(themeConfigFixture));
  vi.mocked(themeSettingsApi.saveConfig).mockImplementation(async (_slug, input) => ({ ...themeConfigFixture, version: 4, config: { ...themeConfigFixture.config, ...input.config }, overrides: input.config }));
  render(<App />);
  fireEvent.click(await screen.findByRole("menuitem", { name: "主题管理" }));
  fireEvent.click(await screen.findByRole("button", { name: "配置主题 Paper" }));
  expect((await screen.findByLabelText("页脚文字") as HTMLTextAreaElement).value).toBe("默认页脚");
  fireEvent.change(screen.getByLabelText("页脚文字"), { target: { value: "" } });
  fireEvent.click(screen.getByRole("switch", { name: "显示描述" }));
  fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
  await waitFor(() => expect(themeSettingsApi.saveConfig).toHaveBeenCalledExactlyOnceWith("paper", {
    id: "paper-record", expected_release: "paper-release", config_schema_version: 1, expected_version: 3, config: { title: "", show: false },
  }));
  expect(themeSettingsApi.save).not.toHaveBeenCalled();
  await screen.findByText(/配置 v4/);
  fireEvent.click(screen.getByRole("button", { name: "恢复默认 页脚文字" }));
  expect((screen.getByLabelText("页脚文字") as HTMLTextAreaElement).value).toBe("默认页脚");
  fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
  await waitFor(() => expect(themeSettingsApi.saveConfig).toHaveBeenLastCalledWith("paper", expect.objectContaining({ expected_version: 4, config: { show: false } })));
});

it("显示字段长度错误，冲突时保留输入并要求重新加载新主题身份", async () => {
  vi.mocked(themeSettingsApi.getConfig).mockResolvedValue(structuredClone(themeConfigFixture));
  vi.mocked(themeSettingsApi.saveConfig).mockRejectedValue(new ApiError(409, "conflict", "version_conflict"));
  render(<App />);
  fireEvent.click(await screen.findByRole("menuitem", { name: "主题管理" }));
  fireEvent.click(await screen.findByRole("button", { name: "配置主题 Paper" }));
  const input = await screen.findByLabelText("页脚文字");
  fireEvent.change(input, { target: { value: "长".repeat(21) } });
  await screen.findByText(/长度应在 0–20/);
  expect(screen.getByRole("button", { name: "保存配置" }).hasAttribute("disabled")).toBe(true);
  fireEvent.change(input, { target: { value: "我的输入" } });
  fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
  await screen.findByText(/当前输入已保留/);
  expect((screen.getByLabelText("页脚文字") as HTMLTextAreaElement).value).toBe("我的输入");
  expect(themeSettingsApi.saveConfig).toHaveBeenCalledTimes(1);
  vi.mocked(themeSettingsApi.getConfig).mockResolvedValue({ ...themeConfigFixture, id: "reinstalled-record", version: 1 });
  fireEvent.click(screen.getByRole("button", { name: "重新加载配置" }));
  await waitFor(() => expect((screen.getByLabelText("页脚文字") as HTMLTextAreaElement).value).toBe("默认页脚"));
  fireEvent.change(screen.getByLabelText("页脚文字"), { target: { value: "新配置" } });
  fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
  await waitFor(() => expect(themeSettingsApi.saveConfig).toHaveBeenLastCalledWith("paper", expect.objectContaining({ id: "reinstalled-record", expected_version: 1 })));
});

it("旧主题无声明时显示空表单且不能保存", async () => {
  vi.mocked(themeSettingsApi.getConfig).mockResolvedValue({ ...themeConfigFixture, fields: [], config: {}, overrides: {} });
  render(<App />);
  fireEvent.click(await screen.findByRole("menuitem", { name: "主题管理" }));
  fireEvent.click(await screen.findByRole("button", { name: "配置主题 Paper" }));
  await screen.findByText("此主题没有可配置字段。");
  expect(screen.getByRole("button", { name: "保存配置" }).hasAttribute("disabled")).toBe(true);
});
