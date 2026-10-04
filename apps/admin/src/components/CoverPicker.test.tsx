import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { QueryClientProvider, useQuery } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { mediaApi } from "../api/media";
import { mediaPageQuery } from "../mediaQueries";
import { createQueryClient } from "../queryClient";
import { invalidateAfterWrite } from "../queryEffects";
import { AdminProviders } from "../providers";
import { CoverPicker } from "./CoverPicker";
import type { MediaAsset, MediaPage } from "../types";

vi.mock("../api/media", async (load) => {
  const original = await load<typeof import("../api/media")>();
  return { ...original, mediaApi: { ...original.mediaApi, list: vi.fn(), upload: vi.fn() } };
});
beforeEach(() => vi.resetAllMocks());
afterEach(cleanup);
const empty: MediaPage = { items: [], total: 0, page: 1, per_page: 24 };
const populated: MediaPage = {
  ...empty,
  total: 1,
  items: [
    {
      id: "m1",
      original_name: "shared.png",
      mime: "image/png",
      byte_size: 16,
      width: 2,
      height: 2,
      deleted_at: null,
      version: 1,
      created_at: "2026-09-29T00:00:00Z",
      owner_id: "u1",
      owner_display: "writer",
      url: "/media/m1",
      reference_count: 0,
    },
  ],
};
function Library() {
  const query = useQuery(mediaPageQuery(1));
  return (
    <output aria-label="library-count">{query.data?.total ?? "loading"}</output>
  );
}
function picker(canReadMedia = true) {
  return (
    <CoverPicker
      value={null}
      onChange={vi.fn()}
      canReadMedia={canReadMedia}
      canUploadMedia={false}
    />
  );
}
it("does not fetch while closed or without media.read", async () => {
  const mounted = render(<AdminProviders>{picker()}</AdminProviders>);
  expect(mediaApi.list).not.toHaveBeenCalled();
  mounted.rerender(<AdminProviders>{picker(false)}</AdminProviders>);
  expect(screen.queryByRole("button", { name: "选择封面" })).toBeNull();
  expect(mediaApi.list).not.toHaveBeenCalled();
});
it("refreshes the library and picker after media changes", async () => {
  vi.mocked(mediaApi.list).mockResolvedValue(populated);
  const client = createQueryClient();
  render(
    <AdminProviders>
      <QueryClientProvider client={client}>
        <Library />
        {picker()}
      </QueryClientProvider>
    </AdminProviders>,
  );
  fireEvent.click(screen.getByRole("button", { name: "选择封面" }));
  await screen.findByText("shared.png");
  expect(screen.getByLabelText("library-count").textContent).toBe("1");
  vi.mocked(mediaApi.list).mockResolvedValue(empty);
  await act(async () => {
    await invalidateAfterWrite(client, "media");
  });
  await waitFor(() =>
    expect(screen.getByLabelText("library-count").textContent).toBe("0"),
  );
  expect(screen.queryByText("shared.png")).toBeNull();
});
it("passes cancellation to fetch when the picker unmounts", async () => {
  vi.mocked(mediaApi.list).mockReturnValue(new Promise(() => {}));
  const mounted = render(<AdminProviders>{picker()}</AdminProviders>);
  fireEvent.click(screen.getByRole("button", { name: "选择封面" }));
  await waitFor(() => expect(mediaApi.list).toHaveBeenCalled());
  const signal = vi.mocked(mediaApi.list).mock.calls[0][2];
  expect(signal?.aborted).toBe(false);
  mounted.unmount();
  expect(signal?.aborted).toBe(true);
});

it("does not select an upload from the previous editing target, but refreshes the library", async () => {
  vi.mocked(mediaApi.list).mockResolvedValue(empty);
  let finishUpload!: (asset: MediaAsset) => void;
  vi.mocked(mediaApi.upload).mockReturnValue(new Promise((resolve) => { finishUpload = resolve; }));
  const onChange = vi.fn();
  const view = (uploadScope: string) => <AdminProviders>
    <Library />
    <CoverPicker value={null} onChange={onChange} canReadMedia canUploadMedia uploadScope={uploadScope} />
  </AdminProviders>;
  const mounted = render(view("post-a"));
  fireEvent.click(screen.getByRole("button", { name: "选择封面" }));
  await waitFor(() => expect(screen.getByLabelText("library-count").textContent).toBe("0"));
  fireEvent.change(document.querySelector('input[type="file"]')!, {
    target: { files: [new File([new Uint8Array(8)], "cover.png", { type: "image/png" })] },
  });
  expect(mediaApi.upload).toHaveBeenCalledTimes(1);
  mounted.rerender(view("post-b"));
  fireEvent.click(screen.getByRole("button", { name: "选择封面" }));
  vi.mocked(mediaApi.list).mockResolvedValue(populated);
  await act(async () => { finishUpload(populated.items[0]); });
  await waitFor(() => expect(screen.getByLabelText("library-count").textContent).toBe("1"));
  expect(onChange).not.toHaveBeenCalled();
  expect(screen.getByRole("dialog")).toBeTruthy();
  expect(screen.getByRole("button", { name: "上传图片" }).hasAttribute("disabled")).toBe(false);
});

it("selects an older image from a later search page", async () => {
  vi.mocked(mediaApi.list).mockImplementation(async (page = 1, _trash, _signal, q = "") => ({
    ...populated, page, per_page: 1, total: 2,
    items: [{ ...populated.items[0], id: `${q}-${page}`, original_name: `${q || 'recent'}-${page}.png` }],
  }));
  const onChange = vi.fn();
  render(<AdminProviders><CoverPicker value={null} onChange={onChange} canReadMedia canUploadMedia={false} /></AdminProviders>);
  fireEvent.click(screen.getByRole('button', { name: '选择封面' }));
  await screen.findByText('recent-1.png');
  fireEvent.change(screen.getByLabelText('搜索图片'), { target: { value: 'cover' } });
  fireEvent.click(screen.getByRole('button', { name: '搜索' }));
  await screen.findByText('cover-1.png');
  fireEvent.click(screen.getByRole('button', { name: '下一页' }));
  await screen.findByText('cover-2.png');
  fireEvent.click(screen.getByRole('button', { name: '选择' }));
  expect(onChange).toHaveBeenCalledWith('cover-2');
});
