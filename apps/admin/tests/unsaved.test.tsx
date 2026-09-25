// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { api, seriesApi } from "../src/api";
import { paths } from "../src/router";
import type { PostDetail } from "../src/types";

/**
 * 未保存离开保护（src/unsaved.tsx + AdminLayout 的菜单拦截）。
 *
 * 覆盖两条真实丢稿路径：点侧边栏去别的屏、刷新/关闭页面。
 * 浏览器前进后退**有意不覆盖**（见 useUnsavedGuard 注释），因此这里也不断言。
 */

const { logoutSpy } = vi.hoisted(() => ({ logoutSpy: vi.fn(async () => {}) }));

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    status: "authenticated",
    me: { user_id: "me", permissions: ["post.create", "post.publish", "tag.manage"] },
    logout: logoutSpy,
  }),
}));

vi.mock("../src/api", async (importOriginal) => {
  const original = await importOriginal<typeof import("../src/api")>();
  return {
    ...original,
    categoryApi: { list: vi.fn() },
    seriesApi: { list: vi.fn() },
    api: {
      getPost: vi.fn(),
      createPost: vi.fn(),
      updatePost: vi.fn(),
      publishPost: vi.fn(),
      unpublishPost: vi.fn(),
      listTags: vi.fn(),
      listPosts: vi.fn(),
      categoryApi: { list: vi.fn() },
    },
  };
});

const post: PostDetail = {
  id: "post-id", slug: "first", title: "原始标题", content: "原始正文",
  excerpt: null, status: "draft", visibility: "public", version: 1,
  published_at: null, updated_at: "2026-09-22T00:00:00Z", author_id: "author-id",
  tag_ids: [], category_id: null, series_id: null, series_order: null,
  cover_media_id: null, cover_url: null,
};

function titleInput(): HTMLInputElement {
  return screen.getByLabelText("标题") as HTMLInputElement;
}

/** 侧边栏菜单项按无障碍名定位。 */
function menuItem(label: string): HTMLElement {
  return screen.getByRole("menuitem", { name: label });
}

/** 让编辑器变成「有未保存改动」。 */
async function openDirtyEditor(): Promise<void> {
  render(<App />);
  await screen.findByDisplayValue(post.title);
  fireEvent.change(titleInput(), { target: { value: "改过的标题" } });
}

beforeEach(() => {
  vi.resetAllMocks();
  window.history.replaceState(null, "", paths.editPost(post.slug));
  vi.mocked(api.getPost).mockResolvedValue(post);
  vi.mocked(api.listTags).mockResolvedValue([]);
  vi.mocked(api.categoryApi.list).mockResolvedValue([]);
  vi.mocked(seriesApi.list).mockResolvedValue([]);
  vi.mocked(api.listPosts).mockResolvedValue([]);
});
afterEach(cleanup);

describe("未保存离开保护", () => {
  it("有未保存改动时点菜单先确认；选「留在此页」不导航且内容保留", async () => {
    await openDirtyEditor();

    fireEvent.click(menuItem("标签"));
    // 按 dialog 的无障碍名断言：antd 的标题是 span 套在标题容器里，按文本查会命中两层。
    expect(await screen.findByRole("dialog", { name: "有未保存的修改" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "留在此页" }));
    await act(async () => {});

    expect(window.location.pathname).toBe(paths.editPost(post.slug));
    expect(titleInput().value).toBe("改过的标题");
  });

  it("确认离开后导航到目标屏", async () => {
    await openDirtyEditor();

    fireEvent.click(menuItem("标签"));
    fireEvent.click(await screen.findByRole("button", { name: "放弃修改并离开" }));

    await waitFor(() => expect(window.location.pathname).toBe(paths.tags));
    // 目标屏真的渲染了（空目录文案），不是只改了地址。
    expect(await screen.findByText(/还没有标签/)).toBeTruthy();
  });

  it("没有未保存改动时直接导航，不弹确认", async () => {
    render(<App />);
    await screen.findByDisplayValue(post.title);

    fireEvent.click(menuItem("标签"));
    await waitFor(() => expect(window.location.pathname).toBe(paths.tags));
    expect(screen.queryByRole("dialog", { name: "有未保存的修改" })).toBeNull();
  });

  it("beforeunload：无改动不拦截，有改动要求确认", async () => {
    render(<App />);
    await screen.findByDisplayValue(post.title);

    const clean = new Event("beforeunload", { cancelable: true });
    window.dispatchEvent(clean);
    expect(clean.defaultPrevented).toBe(false);

    fireEvent.change(titleInput(), { target: { value: "改过的标题" } });
    const dirty = new Event("beforeunload", { cancelable: true }) as unknown as BeforeUnloadEvent;
    window.dispatchEvent(dirty);
    expect(dirty.defaultPrevented).toBe(true);
  });

  it("保存成功后不再拦截离开", async () => {
    vi.mocked(api.updatePost).mockResolvedValue({ ...post, title: "改过的标题", version: 2 });
    await openDirtyEditor();

    fireEvent.click(screen.getByRole("button", { name: "保存并更新线上" }));
    await waitFor(() => expect(api.updatePost).toHaveBeenCalledTimes(1));
    await screen.findByText("已保存（已发布内容直接更新线上）。");

    // 服务端已接受，脏标记应清掉：这时点菜单直接走，不再弹确认。
    fireEvent.click(menuItem("标签"));
    await waitFor(() => expect(window.location.pathname).toBe(paths.tags));
    expect(screen.queryByRole("dialog", { name: "有未保存的修改" })).toBeNull();
  });

  // 回归：编辑页与回收站都被映射到菜单的「文章」这一项，
  // 用「菜单选中项」判断是否已在目标页会让「编辑页 → 列表」点了没反应。
  it("编辑页点侧栏「文章」能返回列表", async () => {
    render(<App />);
    await screen.findByDisplayValue(post.title);

    fireEvent.click(menuItem("文章"));

    await waitFor(() => expect(window.location.pathname).toBe(paths.list));
  });

  // 回归：保护只接在菜单上时，屏内链接一点就走。
  it("屏内「标签目录」链接同样先确认", async () => {
    await openDirtyEditor();

    fireEvent.click(screen.getByRole("link", { name: "标签目录" }));
    expect(await screen.findByRole("dialog", { name: "有未保存的修改" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "留在此页" }));
    await act(async () => {});

    expect(window.location.pathname).toBe(paths.editPost(post.slug));
    expect(titleInput().value).toBe("改过的标题");
  });

  // 回归：退出登录也是离页入口，未保存内容会随会话结束丢失。
  it("有未保存改动时退出登录先确认", async () => {
    await openDirtyEditor();

    fireEvent.click(screen.getByRole("button", { name: "退出登录" }));
    expect(await screen.findByRole("dialog", { name: "有未保存的修改" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "留在此页" }));
    await act(async () => {});

    expect(logoutSpy).not.toHaveBeenCalled();
  });

  it("没有未保存改动时退出登录不再确认", async () => {
    render(<App />);
    await screen.findByDisplayValue(post.title);

    fireEvent.click(screen.getByRole("button", { name: "退出登录" }));

    await waitFor(() => expect(logoutSpy).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("dialog", { name: "有未保存的修改" })).toBeNull();
  });
});
