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
import { mediaApi } from "../api";
import { mediaPageQuery } from "../mediaQueries";
import { createQueryClient } from "../queryClient";
import { invalidateAfterWrite } from "../queryEffects";
import { AdminProviders } from "../providers";
import { CoverPicker } from "./CoverPicker";
import type { MediaPage } from "../types";

vi.mock("../api", async (load) => {
  const actual = await load<typeof import("../api")>();
  return { ...actual, mediaApi: { ...actual.mediaApi, list: vi.fn() } };
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
it("shares the library request and refreshes both observers after invalidation", async () => {
  let resolve!: (page: MediaPage) => void;
  vi.mocked(mediaApi.list)
    .mockReturnValueOnce(
      new Promise((done) => {
        resolve = done;
      }),
    )
    .mockResolvedValue(empty);
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
  await waitFor(() => expect(mediaApi.list).toHaveBeenCalledTimes(1));
  await act(async () => resolve(populated));
  await screen.findByText("shared.png");
  expect(screen.getByLabelText("library-count").textContent).toBe("1");
  await act(async () => {
    await invalidateAfterWrite(client, "media");
  });
  await waitFor(() =>
    expect(screen.getByLabelText("library-count").textContent).toBe("0"),
  );
  expect(screen.queryByText("shared.png")).toBeNull();
  expect(mediaApi.list).toHaveBeenCalledTimes(2);
});
it("passes cancellation to fetch when the picker unmounts", async () => {
  vi.mocked(mediaApi.list).mockReturnValue(new Promise(() => {}));
  const mounted = render(<AdminProviders>{picker()}</AdminProviders>);
  fireEvent.click(screen.getByRole("button", { name: "选择封面" }));
  await waitFor(() => expect(mediaApi.list).toHaveBeenCalledTimes(1));
  const signal = vi.mocked(mediaApi.list).mock.calls[0][2];
  expect(signal?.aborted).toBe(false);
  mounted.unmount();
  expect(signal?.aborted).toBe(true);
});
