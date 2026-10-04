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

  it("全屏退出和卸载恢复页面滚动，输入法 Escape 不丢失写作状态", async () => {
    const mounted = render(<Editor initial="尚未保存的正文" />); await ready();
    const original = input();
    fireEvent.click(screen.getByRole("button", { name: "全屏写作" }));
    expect(document.body.style.overflow).toBe("hidden");
    fireEvent.compositionStart(input());
    fireEvent.keyDown(input(), { key: "Escape", isComposing: true });
    expect(screen.getByRole("button", { name: "退出全屏" })).toBeTruthy();
    fireEvent.compositionEnd(input());
    fireEvent.keyDown(input(), { key: "Escape" });
    expect(screen.getByRole("button", { name: "全屏写作" })).toBeTruthy();
    expect(input()).toBe(original);
    expect(input().value).toBe("尚未保存的正文");
    expect(document.body.style.overflow).toBe("");
    fireEvent.click(screen.getByRole("button", { name: "全屏写作" }));
    mounted.unmount();
    expect(document.body.style.overflow).toBe("");
  });
});
