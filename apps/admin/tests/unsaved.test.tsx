// @vitest-environment jsdom
import { contentPage } from "./contentFixtures";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ConfigProvider } from "antd";
import { App } from "../src/App";
import { postsApi } from "../src/api/posts";
import { tagsApi, categoryApi, seriesApi } from "../src/api/taxonomy";
import { navigate, paths } from "../src/router";
import type { PostDetail } from "../src/types";

/**
 * 未保存离开保护（src/unsaved.tsx + AdminLayout 的菜单拦截）。
 *
 * 覆盖侧栏、屏内链接、退出登录、刷新/关闭，以及真实的 history.go/back/forward。
 * 取消历史导航不能卸载编辑器、重取正文或破坏前进栈。
 */

const { logoutSpy } = vi.hoisted(() => ({ logoutSpy: vi.fn(async () => {}) }));

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    status: "authenticated",
    me: { user_id: "me", permissions: ["post.create", "post.publish", "tag.manage"] },
    logout: logoutSpy,
  }),
}));

vi.mock("../src/api/taxonomy", async (load) => {
  const original = await load<typeof import("../src/api/taxonomy")>();
  return { ...original, categoryApi: { list: vi.fn() }, seriesApi: { list: vi.fn() }, tagsApi: { ...original.tagsApi, listTags: vi.fn() } };
});
vi.mock("../src/api/posts", async (load) => {
  const original = await load<typeof import("../src/api/posts")>();
  return { ...original, postsApi: { ...original.postsApi, getPost: vi.fn(), createPost: vi.fn(), updatePost: vi.fn(), publishPost: vi.fn(), unpublishPost: vi.fn(), listPosts: vi.fn() } };
});

const post: PostDetail = {
  id: "post-id", slug: "first", title: "原始标题", content: "原始正文",
  excerpt: null, status: "draft", visibility: "public", version: 1,
  published_at: null, updated_at: "2026-09-22T00:00:00Z", author_id: "author-id",
  tag_ids: [], category_id: null, series: [],
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
  render(<ConfigProvider theme={{ token: { motion: false } }}><App /></ConfigProvider>);
  await screen.findByDisplayValue(post.title);
  fireEvent.change(titleInput(), { target: { value: "改过的标题" } });
}

beforeEach(() => {
  vi.resetAllMocks();
  window.history.replaceState(null, "", paths.editPost(post.id));
  vi.mocked(postsApi.getPost).mockResolvedValue(post);
  vi.mocked(tagsApi.listTags).mockResolvedValue([]);
  vi.mocked(categoryApi.list).mockResolvedValue([]);
  vi.mocked(seriesApi.list).mockResolvedValue([]);
  vi.mocked(postsApi.listPosts).mockResolvedValue(contentPage([]));
});
afterEach(cleanup);

describe("未保存离开保护", () => {
  it("后退取消保留完整表单，再次确认后退和前进仍走原历史项", async () => {
    window.history.replaceState(null, "", paths.list);
    render(<ConfigProvider theme={{ token: { motion: false } }}><App /></ConfigProvider>);
    await screen.findByText(/还没有文章/);
    act(() => navigate(paths.editPost(post.id)));
    await screen.findByDisplayValue(post.title);
    const title = titleInput();
    const body = screen.getByLabelText("正文（Markdown）") as HTMLTextAreaElement;
    fireEvent.change(title, { target: { value: "未保存的标题" } });
    fireEvent.change(body, { target: { value: "未保存的正文\n\n第二段" } });
    const length = window.history.length;

    act(() => window.history.back());
    await screen.findByRole("dialog", { name: "有未保存的修改" });
    expect(window.location.pathname).toBe(paths.editPost(post.id));
    expect(titleInput()).toBe(title);
    fireEvent.click(screen.getByRole("button", { name: "留在此页" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(title.value).toBe("未保存的标题");
    expect(body.value).toBe("未保存的正文\n\n第二段");
    expect(postsApi.getPost).toHaveBeenCalledTimes(1);
    expect(window.history.length).toBe(length);

    act(() => window.history.back());
    fireEvent.click(await screen.findByRole("button", { name: "放弃修改并离开" }));
    await screen.findByText(/还没有文章/);
    expect(window.location.pathname).toBe(paths.list);
    act(() => window.history.forward());
    await screen.findByDisplayValue(post.title);
    expect(window.location.pathname).toBe(paths.editPost(post.id));
    expect(window.history.length).toBe(length);
  });

  it("前进同样保护正文，取消不会丢掉原前进目标", async () => {
    render(<ConfigProvider theme={{ token: { motion: false } }}><App /></ConfigProvider>);
    await screen.findByDisplayValue(post.title);
    act(() => navigate(paths.tags));
    await screen.findByText(/还没有标签/);
    act(() => window.history.back());
    await screen.findByDisplayValue(post.title);
    fireEvent.change(titleInput(), { target: { value: "回到编辑器继续写" } });

    act(() => window.history.forward());
    fireEvent.click(await screen.findByRole("button", { name: "留在此页" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(titleInput().value).toBe("回到编辑器继续写");
    expect(window.location.pathname).toBe(paths.editPost(post.id));
    act(() => window.history.forward());
    fireEvent.click(await screen.findByRole("button", { name: "放弃修改并离开" }));
    await screen.findByText(/还没有标签/);
    expect(window.location.pathname).toBe(paths.tags);
  });

  it("跨多个历史项后退时只确认一次，反复点击后退不会卸载表单", async () => {
    window.history.replaceState(null, "", paths.tags);
    render(<ConfigProvider theme={{ token: { motion: false } }}><App /></ConfigProvider>);
    await screen.findByText(/还没有标签/);
    act(() => navigate(paths.list));
    await screen.findByText(/还没有文章/);
    act(() => navigate(paths.editPost(post.id)));
    await screen.findByDisplayValue(post.title);
    fireEvent.change(titleInput(), { target: { value: "跨多页仍保留" } });
    act(() => window.history.go(-2));
    await screen.findByRole("dialog", { name: "有未保存的修改" });
    const currentState = window.history.state;
    // 对话框打开时再按一次后退；两个 popstate 分别是退一步和恢复原位。
    await act(async () => {
      await new Promise<void>(resolve => {
        const restored = () => {
          if (window.location.pathname !== paths.editPost(post.id)) return;
          window.removeEventListener("popstate", restored);
          resolve();
        };
        window.addEventListener("popstate", restored);
        window.history.back();
      });
    });
    expect(window.history.state).toEqual(currentState);
    expect(screen.getAllByRole("dialog")).toHaveLength(1);
    expect(titleInput().value).toBe("跨多页仍保留");
    expect(postsApi.getPost).toHaveBeenCalledTimes(1);
    fireEvent.click(screen.getByRole("button", { name: "放弃修改并离开" }));
    await screen.findByText(/还没有标签/);
    expect(window.location.pathname).toBe(paths.tags);
    act(() => window.history.go(2));
    await screen.findByDisplayValue(post.title);
    expect(window.location.pathname).toBe(paths.editPost(post.id));
  });

  it("保存完成时取消待决离开，保存期间继续输入仍受后退保护", async () => {
    window.history.replaceState(null, "", paths.list);
    let finish!: (value: PostDetail) => void;
    vi.mocked(postsApi.createPost).mockReturnValue(new Promise(resolve => { finish = resolve; }));
    render(<ConfigProvider theme={{ token: { motion: false } }}><App /></ConfigProvider>);
    await screen.findByText(/还没有文章/);
    act(() => navigate(paths.newPost));
    await screen.findByLabelText("标题");
    fireEvent.change(titleInput(), { target: { value: post.title } });
    fireEvent.click(screen.getByRole("button", { name: /保存草稿|保存预约内容|更新已发布内容/ }));
    await waitFor(() => expect(postsApi.createPost).toHaveBeenCalledTimes(1));
    fireEvent.change(screen.getByLabelText("正文（Markdown）"), { target: { value: "保存期间的新正文" } });
    act(() => window.history.back());
    await screen.findByRole("dialog", { name: "有未保存的修改" });
    await act(async () => finish({ ...post, content: "" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(window.location.pathname).toBe(paths.editPost(post.id));
    expect((screen.getByLabelText("正文（Markdown）") as HTMLTextAreaElement).value).toBe("保存期间的新正文");
    expect(postsApi.getPost).not.toHaveBeenCalled();
    act(() => window.history.back());
    fireEvent.click(await screen.findByRole("button", { name: "留在此页" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(window.location.pathname).toBe(paths.editPost(post.id));
    expect((screen.getByLabelText("正文（Markdown）") as HTMLTextAreaElement).value).toBe("保存期间的新正文");
  });

  it("有未保存改动时点菜单先确认；选「留在此页」不导航且内容保留", async () => {
    await openDirtyEditor();

    fireEvent.click(menuItem("标签"));
    // 按 dialog 的无障碍名断言：antd 的标题是 span 套在标题容器里，按文本查会命中两层。
    expect(await screen.findByRole("dialog", { name: "有未保存的修改" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "留在此页" }));
    await act(async () => {});

    expect(window.location.pathname).toBe(paths.editPost(post.id));
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
    render(<ConfigProvider theme={{ token: { motion: false } }}><App /></ConfigProvider>);
    await screen.findByDisplayValue(post.title);

    fireEvent.click(menuItem("标签"));
    await waitFor(() => expect(window.location.pathname).toBe(paths.tags));
    expect(screen.queryByRole("dialog", { name: "有未保存的修改" })).toBeNull();
  });

  it("beforeunload：无改动不拦截，有改动要求确认", async () => {
    render(<ConfigProvider theme={{ token: { motion: false } }}><App /></ConfigProvider>);
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
    vi.mocked(postsApi.updatePost).mockResolvedValue({ ...post, title: "改过的标题", version: 2 });
    await openDirtyEditor();

    fireEvent.click(screen.getByRole("button", { name: /保存草稿|保存预约内容|更新已发布内容/ }));
    await waitFor(() => expect(postsApi.updatePost).toHaveBeenCalledTimes(1));
    await screen.findByText("已保存。");

    // 服务端已接受，脏标记应清掉：这时点菜单直接走，不再弹确认。
    fireEvent.click(menuItem("标签"));
    await waitFor(() => expect(window.location.pathname).toBe(paths.tags));
    expect(screen.queryByRole("dialog", { name: "有未保存的修改" })).toBeNull();
  });

  // 回归：编辑页与回收站都被映射到菜单的「文章」这一项，
  // 用「菜单选中项」判断是否已在目标页会让「编辑页 → 列表」点了没反应。
  it("编辑页点侧栏「文章」能返回列表", async () => {
    render(<ConfigProvider theme={{ token: { motion: false } }}><App /></ConfigProvider>);
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

    expect(window.location.pathname).toBe(paths.editPost(post.id));
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
    render(<ConfigProvider theme={{ token: { motion: false } }}><App /></ConfigProvider>);
    await screen.findByDisplayValue(post.title);

    fireEvent.click(screen.getByRole("button", { name: "退出登录" }));

    await waitFor(() => expect(logoutSpy).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("dialog", { name: "有未保存的修改" })).toBeNull();
  });
});
