// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { postsApi } from "../src/api/posts";
import { tagsApi, categoryApi, seriesApi } from "../src/api/taxonomy";
import { pagesApi } from "../src/api/pages";
import { mediaApi } from "../src/api/media";
import { navigate, paths } from "../src/router";
import type { MediaAsset, PostDetail } from "../src/types";

/** 每个用例可改写的权限集合（`media.read` 决定面板入口是否存在）。 */
const state = vi.hoisted(() => ({
  permissions: ["post.update", "media.read", "media.upload"] as string[],
}));

vi.mock("../src/auth", () => ({
  useAuth: () => ({
    status: "authenticated",
    me: { user_id: "me", permissions: state.permissions },
  }),
}));

vi.mock("../src/api/taxonomy", async (load) => {
  const original = await load<typeof import("../src/api/taxonomy")>();
  return { ...original, categoryApi: { list: vi.fn() }, seriesApi: { list: vi.fn() }, tagsApi: { ...original.tagsApi, listTags: vi.fn() } };
});
vi.mock("../src/api/media", async (load) => {
  const original = await load<typeof import("../src/api/media")>();
  return { ...original, mediaApi: { list: vi.fn(), detail: vi.fn(), upload: vi.fn(), remove: vi.fn() } };
});
vi.mock("../src/api/posts", async (load) => {
  const original = await load<typeof import("../src/api/posts")>();
  return { ...original, postsApi: { ...original.postsApi, getPost: vi.fn(), createPost: vi.fn(), updatePost: vi.fn(), publishPost: vi.fn(), unpublishPost: vi.fn() } };
});
vi.mock("../src/api/pages", async (load) => {
  const original = await load<typeof import("../src/api/pages")>();
  return { ...original, pagesApi: { ...original.pagesApi, getPage: vi.fn(), createPage: vi.fn() } };
});

const post: PostDetail = {
  id: "post-id",
  slug: "first",
  title: "原始标题",
  content: "原始正文",
  excerpt: null,
  status: "draft",
  visibility: "public",
  version: 1,
  published_at: null,
  updated_at: "2026-09-23T00:00:00Z",
  author_id: "me",
  tag_ids: [],
  category_id: null,
  series: [],
  cover_media_id: null,
  cover_url: null,
};

function asset(overrides: Partial<MediaAsset> = {}): MediaAsset {
  return {
    id: "media-1",
    original_name: "photo.png",
    mime: "image/png",
    byte_size: 1024,
    width: 100,
    height: 50,
    deleted_at: null,
    version: 2,
    created_at: "2026-09-23T10:00:00Z",
    owner_id: "me",
    owner_display: "sun",
    url: "/media/media-1",
    reference_count: 0,
    ...overrides,
  };
}

function contentBox(): HTMLTextAreaElement {
  return screen.getByLabelText("正文（Markdown）") as HTMLTextAreaElement;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

/** 打开图片面板并等待资产列表就绪。 */
async function openPanel(): Promise<void> {
  fireEvent.click(screen.getByRole("button", { name: "插入图片" }));
  await waitFor(() => expect(screen.getByText("photo.png")).toBeTruthy());
}

beforeEach(() => {
  vi.resetAllMocks();
  state.permissions = ["post.update", "media.read", "media.upload"];
  window.history.replaceState(null, "", paths.editPost(post.id));
  vi.mocked(postsApi.getPost).mockResolvedValue(post);
  vi.mocked(tagsApi.listTags).mockResolvedValue([]);
  vi.mocked(categoryApi.list).mockResolvedValue([]);
  vi.mocked(seriesApi.list).mockResolvedValue([]);
  vi.mocked(mediaApi.list).mockResolvedValue({
    items: [asset()],
    total: 1,
    page: 1,
    per_page: 24,
  });
});
afterEach(cleanup);

describe("编辑器内插入图片", () => {
  it("点击插入把站内图片写进正文光标处", async () => {
    render(<App />);
    await waitFor(() => expect(contentBox().value).toBe("原始正文"));

    // 把光标放到正文中间之后插入。
    const box = contentBox();
    fireEvent.change(box, { target: { value: "AB" } });
    box.setSelectionRange(1, 1);

    await openPanel();
    fireEvent.click(screen.getByRole("button", { name: "插入" }));

    await waitFor(() =>
      expect(contentBox().value).toBe("A\n\n![photo.png](/media/media-1)\n\nB"),
    );
    // 插入只改本地表单：必须显式保存才落库。
    expect(postsApi.updatePost).not.toHaveBeenCalled();
  });

  it("跨页搜索旧图片后保留正文光标和替代文字，不触发保存", async () => {
    vi.mocked(mediaApi.list).mockImplementation(async (page = 1, _trash, _signal, q = "") => ({
      items: [asset(q ? {
        id: `found-${page}`, original_name: `山间-${page}.png`, url: `/media/found-${page}`,
      } : page === 1 ? {} : { id: 'older', original_name: 'older.png' })],
      page, per_page: 1, total: 2,
    }));
    render(<App />);
    await waitFor(() => expect(contentBox().value).toBe("原始正文"));
    fireEvent.change(contentBox(), { target: { value: 'AB' } });
    contentBox().setSelectionRange(1, 1);
    await openPanel();
    fireEvent.change(screen.getByLabelText('替代文字'), { target: { value: '历史插图' } });
    fireEvent.click(screen.getByRole('button', { name: '下一页' }));
    await screen.findByText('older.png');
    fireEvent.change(screen.getByLabelText('搜索图片'), { target: { value: ' 山间 ' } });
    expect(fireEvent.keyDown(screen.getByLabelText('搜索图片'), { key: 'Enter', code: 'Enter' })).toBe(false);
    await screen.findByText('山间-1.png');
    expect(mediaApi.list).toHaveBeenLastCalledWith(1, false, expect.any(AbortSignal), '山间');
    fireEvent.click(screen.getByRole('button', { name: '下一页' }));
    await screen.findByText('山间-2.png');
    fireEvent.click(screen.getByRole('button', { name: '插入' }));
    await waitFor(() => expect(contentBox().value).toBe('A\n\n![历史插图](/media/found-2)\n\nB'));
    expect(postsApi.updatePost).not.toHaveBeenCalled();
  });

  it("替代文字非空时写入 alt 而不是文件名", async () => {
    render(<App />);
    await waitFor(() => expect(contentBox().value).toBe("原始正文"));
    fireEvent.change(contentBox(), { target: { value: "" } });
    contentBox().setSelectionRange(0, 0);

    await openPanel();
    fireEvent.change(screen.getByLabelText("替代文字"), {
      target: { value: "山间日出" },
    });
    fireEvent.click(screen.getByRole("button", { name: "插入" }));

    await waitFor(() => expect(contentBox().value).toBe("![山间日出](/media/media-1)"));
  });

  it("粘贴剪贴板图片会上传并插入", async () => {
    vi.mocked(mediaApi.upload).mockResolvedValue(asset({ id: "pasted", url: "/media/pasted", original_name: "pasted.png" }));
    render(<App />);
    await waitFor(() => expect(contentBox().value).toBe("原始正文"));
    fireEvent.change(contentBox(), { target: { value: "" } });

    expect(screen.getByText(/上传后图片链接立即公开/)).toBeTruthy();
    const file = new File([new Uint8Array(8)], "pasted.png", { type: "image/png" });
    fireEvent.paste(contentBox(), { clipboardData: { files: [file] } });

    await waitFor(() => expect(mediaApi.upload).toHaveBeenCalledWith(file));
    await waitFor(() => expect(contentBox().value).toBe("![pasted](/media/pasted)"));
  });

  it.each(["post", "page"] as const)("%s 切换目标后忽略旧上传，新稿保存获得 ID 后仍可插图", async (kind) => {
    state.permissions.push("post.create", "page.create", "page.update");
    const editPath = kind === "post" ? paths.editPost : paths.editPage;
    const newPath = kind === "post" ? paths.newPost : paths.newPage;
    const create = kind === "post" ? postsApi.createPost : pagesApi.createPage;
    window.history.replaceState(null, "", editPath(post.id));
    vi.mocked(pagesApi.getPage).mockResolvedValue(post);
    const oldUpload = deferred<MediaAsset>();
    const newUpload = deferred<MediaAsset>();
    const save = deferred<PostDetail>();
    vi.mocked(mediaApi.upload).mockReturnValueOnce(oldUpload.promise).mockReturnValueOnce(newUpload.promise);
    vi.mocked(create).mockReturnValue(save.promise);
    render(<App />);
    await waitFor(() => expect(contentBox().value).toBe(post.content));
    await openPanel();
    const file = new File([new Uint8Array(8)], "pasted.png", { type: "image/png" });
    fireEvent.paste(contentBox(), { clipboardData: { files: [file] } });

    act(() => { navigate(newPath); });
    await waitFor(() => expect(contentBox().value).toBe(""));
    fireEvent.change(screen.getByLabelText("标题"), { target: { value: "新稿" } });
    fireEvent.change(contentBox(), { target: { value: "新稿正文" } });
    fireEvent.paste(contentBox(), { clipboardData: { files: [file] } });
    expect(mediaApi.upload).toHaveBeenCalledTimes(2);

    const uploaded = asset({ original_name: "old-target.png" });
    vi.mocked(mediaApi.list).mockResolvedValue({ items: [uploaded], total: 1, page: 1, per_page: 24 });
    await act(async () => { oldUpload.resolve(uploaded); });
    expect(contentBox().value).toBe("新稿正文");
    expect(screen.queryByText(/已插入.*张图片/)).toBeNull();
    expect(screen.getByRole("button", { name: "上传中…" }).hasAttribute("disabled")).toBe(true);
    await screen.findByText("old-target.png");

    fireEvent.click(screen.getByRole("button", { name: "保存草稿" }));
    await waitFor(() => expect(create).toHaveBeenCalledTimes(1));
    fireEvent.change(contentBox(), { target: { value: "保存期间继续输入" } });
    await act(async () => { save.resolve({ ...post, id: "created", title: "新稿", content: "新稿正文" }); });
    expect(window.location.pathname).toBe(editPath("created"));
    expect(contentBox().value).toBe("保存期间继续输入");
    contentBox().setSelectionRange(contentBox().value.length, contentBox().value.length);
    await act(async () => { newUpload.resolve(asset({ original_name: "current.png", url: "/media/current" })); });
    expect(contentBox().value).toBe("保存期间继续输入\n\n![current](/media/current)");
  });

  it("拖入图片与粘贴共用同一插入路径", async () => {
    vi.mocked(mediaApi.upload).mockResolvedValue(asset({ id: "dropped", url: "/media/dropped", original_name: "dropped.png" }));
    render(<App />);
    await waitFor(() => expect(contentBox().value).toBe("原始正文"));
    fireEvent.change(contentBox(), { target: { value: "" } });

    const file = new File([new Uint8Array(8)], "dropped.png", { type: "image/png" });
    fireEvent.drop(contentBox(), { dataTransfer: { files: [file] } });

    await waitFor(() => expect(contentBox().value).toBe("![dropped](/media/dropped)"));
  });

  it("不支持的文件类型被客户端拦下，不发上传请求", async () => {
    render(<App />);
    await waitFor(() => expect(contentBox().value).toBe("原始正文"));

    const svg = new File([new Uint8Array(8)], "evil.svg", { type: "image/svg+xml" });
    fireEvent.paste(contentBox(), { clipboardData: { files: [svg] } });

    await waitFor(() => expect(screen.getByText(/不支持的图片类型/)).toBeTruthy());
    expect(mediaApi.upload).not.toHaveBeenCalled();
  });

  it("没有 media.read 时不显示图片入口", async () => {
    state.permissions = ["post.update"];
    render(<App />);
    await waitFor(() => expect(contentBox().value).toBe("原始正文"));
    expect(screen.queryByRole("button", { name: "插入图片" })).toBeNull();
  });

  it("没有 media.read 时封面选择器不提供死路径，只说明缺哪项权限", async () => {
    state.permissions = ["post.update"];
    render(<App />);
    await waitFor(() => expect(contentBox().value).toBe("原始正文"));
    expect(screen.queryByRole("button", { name: "选择封面" })).toBeNull();
    expect(screen.getByText(/没有 media.read 权限/)).toBeTruthy();
  });

  it("没有 media.upload 时面板不提供上传按钮", async () => {
    state.permissions = ["post.update", "media.read"];
    render(<App />);
    await waitFor(() => expect(contentBox().value).toBe("原始正文"));

    await openPanel();
    expect(screen.queryByRole("button", { name: "上传图片" })).toBeNull();
    // 仍可插入已有图片。
    expect(screen.getByRole("button", { name: "插入" })).toBeTruthy();
  });

  it("页面编辑器同样支持插入（Page 无作者，共用同一实现）", async () => {
    window.history.replaceState(null, "", paths.editPage("page-id"));
    vi.mocked(pagesApi.getPage).mockResolvedValue({
      id: "page-id",
      slug: "about",
      title: "关于",
      content: "",
      status: "draft",
      visibility: "public",
      version: 1,
      published_at: null,
      updated_at: "2026-09-23T00:00:00Z",
    });
    render(<App />);

    const box = await waitFor(() => {
      const element = screen.getByLabelText(/正文/) as HTMLTextAreaElement;
      expect(element.value).toBe("");
      return element;
    });
    box.setSelectionRange(0, 0);

    expect(screen.getByText(/上传后图片链接立即公开/)).toBeTruthy();
    // 面板与文章编辑器同一实现：入口文案与插入结果一致。
    fireEvent.click(screen.getByRole("button", { name: "插入图片" }));
    await waitFor(() => expect(screen.getByText("photo.png")).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "插入" }));

    await waitFor(() =>
      expect((screen.getByLabelText(/正文/) as HTMLTextAreaElement).value).toBe(
        "![photo.png](/media/media-1)",
      ),
    );
  });
});
