import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { contentApi } from "../api/content";
import { PublicationPreview } from "./PublicationPreview";

vi.mock("../api/content", () => ({ contentApi: { previewContent: vi.fn() } }));
beforeEach(() => vi.resetAllMocks());
afterEach(cleanup);

it("previews only on request, using current input and an isolated document", async () => {
  let source = "old";
  vi.mocked(contentApi.previewContent).mockResolvedValue({ content_html: "<p>clean</p>", head_html: "" });
  render(<PublicationPreview content="latest" readContent={() => source} />);
  expect(contentApi.previewContent).not.toHaveBeenCalled();
  source = "latest";
  fireEvent.click(screen.getByRole("button", { name: "发布效果预览" }));
  const frame = await screen.findByTitle("发布正文预览");
  expect(contentApi.previewContent).toHaveBeenCalledWith("latest");
  expect(frame.getAttribute("sandbox")).toBe("allow-scripts");
  expect(frame.getAttribute("srcdoc")).toContain("<p>clean</p>");
  expect(frame.getAttribute("srcdoc")).toContain("connect-src 'none'");
});

it("rejects a late response for text that has since changed", async () => {
  let source = "first";
  let finish!: (value: { content_html: string; head_html: string }) => void;
  vi.mocked(contentApi.previewContent).mockReturnValue(new Promise(resolve => { finish = resolve; }));
  render(<PublicationPreview content={source} readContent={() => source} />);
  fireEvent.click(screen.getByRole("button", { name: "发布效果预览" }));
  source = "new input";
  await act(async () => { finish({ content_html: "<p>old</p>", head_html: "" }); });
  expect(await screen.findByText("正文已变化，请重新预览最新内容。")).toBeTruthy();
  expect(screen.queryByTitle("发布正文预览")).toBeNull();
});

it("clears old output on failure and permits an explicit retry", async () => {
  vi.mocked(contentApi.previewContent).mockRejectedValueOnce(new Error("预览失败"))
    .mockResolvedValueOnce({ content_html: "<p>recovered</p>", head_html: "" });
  render(<PublicationPreview content="source" readContent={() => "source"} />);
  fireEvent.click(screen.getByRole("button", { name: "发布效果预览" }));
  await screen.findByText("预览失败");
  expect(screen.queryByTitle("发布正文预览")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "重新预览" }));
  await waitFor(() => expect(screen.getByTitle("发布正文预览").getAttribute("srcdoc")).toContain("recovered"));
});
