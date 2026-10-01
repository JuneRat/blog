import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Grid } from "antd";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MarkdownEditor } from "../src/components/MarkdownEditor";
import { AdminProviders } from "../src/providers";

const attach = () => {};
function Editor({ initial = "", disabled = false }: {
  initial?: string; disabled?: boolean;
}) {
  const [value, setValue] = useState(initial);
  return <AdminProviders><MarkdownEditor
    id="markdown" value={value} onChange={setValue} contentRef={attach} editorScope="test"
    disabled={disabled} /></AdminProviders>;
}
const input = () => screen.getByLabelText("正文（Markdown）") as HTMLTextAreaElement;
const type = (value: string) => fireEvent.change(input(), { target: { value } });
const ready = () => waitFor(() => {
  expect((screen.getByRole("radio", { name: "即时渲染" }) as HTMLInputElement).disabled).toBe(false);
});

beforeEach(() => {
  vi.spyOn(Grid, "useBreakpoint").mockReturnValue({ md: true });
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("Vditor 表单边界", () => {
  it("切换写作模式仍保留表单中的源码", async () => {
    render(<Editor initial="初稿" />); await ready();
    type("**修改后的正文**");
    for (const name of ["即时渲染", "源码", "双栏"]) {
      const mode = screen.getByRole("radio", { name }) as HTMLInputElement;
      fireEvent.click(mode);
      expect(mode.checked).toBe(true);
    }
    expect(input().value).toBe("**修改后的正文**");
  });

  it("加载目标和只读状态仍由现有表单控制", async () => {
    const mounted = render(<Editor initial="正文" disabled />); await ready();
    expect(input().disabled).toBe(true);
    mounted.rerender(<Editor initial="正文" />);
    expect(input().disabled).toBe(false);
    expect(input().value).toBe("正文");
  });
});
