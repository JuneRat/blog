// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError, mediaApi, settingsApi, themeSettingsApi } from "../src/api";
import { paths } from "../src/router";
import type { SiteSettings } from "../src/types";

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    status: "authenticated",
    me: { permissions: ["settings.manage", "media.read", "media.upload"] },
  }),
}));

vi.mock("../src/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("../src/api")>();
  return {
    ...original,
    settingsApi: { get: vi.fn(), save: vi.fn() },
    themeSettingsApi: { get: vi.fn(), save: vi.fn() },
    mediaApi: { ...original.mediaApi, list: vi.fn(), upload: vi.fn() },
  };
});

const fallbackView: SiteSettings = {
  title: "环境变量站点",
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
  status: "ready",
  version: 2,
  created_at: "2026-01-01",
  owner_id: "u1",
  owner_display: "管理员",
  url: "/media/logo-1",
  reference_count: 0,
  public_reference_count: 0,
};

beforeEach(() => {
  vi.resetAllMocks();
  window.history.replaceState(null, "", paths.settings);
  vi.mocked(settingsApi.get).mockResolvedValue(fallbackView);
  vi.mocked(themeSettingsApi.get).mockResolvedValue({ slug: "default", effective_slug: "default", source: "fallback", version: 0, available: [{ slug: "default", name: "Default" }, { slug: "paper", name: "Paper" }] });
});
afterEach(cleanup);

describe("站点设置屏", () => {
  it("切换主题携带版本并显示即时生效", async () => {
    vi.mocked(themeSettingsApi.save).mockResolvedValue({ slug: "paper", effective_slug: "paper", source: "database", version: 1, available: [{ slug: "default", name: "Default" }, { slug: "paper", name: "Paper" }] });
    render(<App />);
    // 主题下拉是 antd Select，不是原生控件：fireEvent.change 改不动它的值。
    // 先等主题加载完成（否则展开的是空列表），再 mouseDown 展开、点选项文案。
    await screen.findByText(/当前主题：Default/);
    const select = await screen.findByLabelText("选择主题");
    fireEvent.mouseDown(select);
    fireEvent.click(await screen.findByTitle("Paper"));
    fireEvent.click(screen.getByRole("button", { name: "切换主题" }));
    await waitFor(() => expect(themeSettingsApi.save).toHaveBeenCalledWith("paper", 0));
    await waitFor(() => expect(screen.getByText(/主题已切换为「Paper」/)).toBeTruthy());
  });

  it("已保存主题缺失时提示默认主题并允许修复", async () => {
    vi.mocked(themeSettingsApi.get).mockResolvedValue({ slug: "removed", effective_slug: "default", source: "database", version: 3, available: [{ slug: "default", name: "Default" }, { slug: "paper", name: "Paper" }] });
    vi.mocked(themeSettingsApi.save).mockResolvedValue({ slug: "default", effective_slug: "default", source: "database", version: 4, available: [{ slug: "default", name: "Default" }, { slug: "paper", name: "Paper" }] });
    render(<App />);
    expect(await screen.findByText(/已保存的主题「removed」当前未安装/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "切换主题" }));
    await waitFor(() => expect(themeSettingsApi.save).toHaveBeenCalledWith("default", 3));
  });
  it("加载生效值并标注来源为环境变量/默认值", async () => {
    render(<App />);
    const title = (await screen.findByLabelText("站点标题")) as HTMLInputElement;
    expect(title.value).toBe("环境变量站点");
    expect(
      screen.getByText(/环境变量\/默认值（数据库尚未配置；保存后由数据库接管）/),
    ).toBeTruthy();
  });

  it("保存携带当前版本并展示数据库来源提示", async () => {
    vi.mocked(settingsApi.save).mockResolvedValue({
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
      expect(settingsApi.save).toHaveBeenCalledWith({
        title: "数据库站点",
        description: "新描述",
        logo_media_id: null,
        expected_version: 0,
      }),
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
        title: "我的标题",
        description: "描述",
        source: "database",
        version: 3,
      });
    // 冲突后的重载返回服务器最新视图（v2，标题已被别处修改）。
    vi.mocked(settingsApi.get)
      .mockResolvedValueOnce({
        title: "第一版",
        description: "描述",
        source: "database",
        version: 1,
      })
      .mockResolvedValueOnce({
        title: "别处的修改",
        description: "描述",
        source: "database",
        version: 2,
      })
      // 保存成功会让站点设置查询失效并在后台重取（这里补上兜底返回值，
      // 避免那次后台读取拿到 undefined）。
      .mockResolvedValue({
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
      expect(settingsApi.save).toHaveBeenLastCalledWith({
        title: "我的标题",
        description: "描述",
        expected_version: 2,
      }),
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

    fireEvent.click(screen.getByRole("button", { name: "重新加载" }));
    await waitFor(() =>
      expect((screen.getByLabelText("站点标题") as HTMLInputElement).value).toBe("服务器标题"),
    );
    expect(screen.getByText(/已重新加载服务器当前值/)).toBeTruthy();
    expect(settingsApi.save).toHaveBeenCalledTimes(1, "重新加载本身不再保存");
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
    await waitFor(() => expect(settingsApi.save).toHaveBeenCalledTimes(1));

    // 请求未回来之前继续编辑：这段输入不能被响应覆盖。
    fireEvent.change(title, { target: { value: "等待期间的新输入" } });
    await act(async () => {
      resolveSave({
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

  it("长度按 Unicode 码点计：200 个 emoji 通过，201 个被拦下", async () => {
    vi.mocked(settingsApi.save).mockResolvedValue({
      title: "😀".repeat(200),
      description: "回退描述",
      source: "database",
      version: 1,
    });
    render(<App />);

    const title = (await screen.findByLabelText("站点标题")) as HTMLInputElement;
    // 原生 maxLength 按 UTF-16 代码单元截断，无法表达码点上限，因此不设该属性。
    expect(title.getAttribute("maxlength")).toBeNull();

    fireEvent.change(title, { target: { value: "😀".repeat(200) } });
    expect(screen.getByText(`200/200 字符`)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(settingsApi.save).toHaveBeenCalledTimes(1));
    expect(vi.mocked(settingsApi.save).mock.calls[0][0].title).toBe("😀".repeat(200));

    fireEvent.change(title, { target: { value: "😀".repeat(201) } });
    expect(screen.getByText(`201/200 字符`)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() =>
      expect(screen.getByText("站点标题长度不能超过 200 字符。")).toBeTruthy(),
    );
    expect(settingsApi.save).toHaveBeenCalledTimes(1);
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

    await waitFor(() => expect(settingsApi.save).toHaveBeenCalledTimes(1));
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

    await waitFor(() => expect(settingsApi.save).toHaveBeenCalledTimes(1));
    expect(vi.mocked(settingsApi.save).mock.calls[0][0]).toMatchObject({
      logo_media_id: null,
      expected_version: 3,
    });
  });
});
