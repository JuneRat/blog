import { Alert, Button, Flex, Grid, Input, Segmented, Spin, Typography, theme } from "antd";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { AriaAttributes, CSSProperties } from "react";
import type Vditor from "vditor";
import type { MarkdownEditorHandle, EditorImage } from "./editorHandle";
import { linkEditorScroll } from "./editorScroll";
import { editorCdn, loadVditor } from "./vditorRuntime";
import { insertImageMarkdown } from "../media";
import "vditor/dist/index.css";
import "./markdownEditor.css";

type Mode = "source" | "split" | "ir";
// Vditor.getValue() appends a newline even in SV; preserve source bytes in source modes.
const readMarkdown = (instance: Vditor) => instance.getCurrentMode() === "sv"
  ? instance.vditor.sv!.element.value : instance.getValue();
interface MarkdownEditorProps extends AriaAttributes {
  id?: string;
  value?: string;
  onChange?: (value: string) => void;
  disabled: boolean;
  editorScope: string;
  contentRef: (node: MarkdownEditorHandle | null) => void;
  onInsertFiles?: (files: File[]) => Promise<boolean>;
  onOpenMedia?: () => void;
  mediaOpen?: boolean;
}

/** Vditor owns editing DOM; the existing Form and account-scoped drafts own Markdown. */
export function MarkdownEditor(props: MarkdownEditorProps) {
  const { id, value = "", disabled, editorScope, contentRef } = props;
  const { token } = theme.useToken();
  const screens = Grid.useBreakpoint();
  const [mode, setMode] = useState<Mode>("split");
  const effectiveMode = !screens.md && mode === "split" ? "source" : mode;
  const [composing, setComposing] = useState(false);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const host = useRef<HTMLDivElement>(null);
  const fallback = useRef<HTMLTextAreaElement | null>(null);
  const editor = useRef<Vditor | null>(null);
  const selection = useRef<Range | null>(null);
  const latest = useRef({ ...props, value, effectiveMode });
  latest.current = { ...props, value, effectiveMode };
  // Lute normalizes Markdown in IR. Only real edits are propagated, not this normalization.
  const observed = useRef("");
  const applied = useRef("");
  const scope = useRef(editorScope);

  function publish() {
    const instance = editor.current;
    if (!instance || latest.current.disabled) return;
    const next = readMarkdown(instance);
    if (observed.current === next) return;
    observed.current = next;
    applied.current = next;
    latest.current.onChange?.(next);
  }

  function configure(instance: Vditor) {
    const { effectiveMode: view, value: source, disabled: readOnly, id: inputId } = latest.current;
    const type = view === "ir" ? "ir" : "sv";
    if (instance.getCurrentMode() !== type) {
      const button = instance.vditor.toolbar!.elements!["edit-mode"].querySelector<HTMLButtonElement>(`[data-mode="${type}"]`)!;
      button.dispatchEvent(new Event(navigator.userAgent.includes("iPhone") ? "touchstart" : "click", { bubbles: true, cancelable: true }));
      // A view change alone must not rewrite the stored source or mark the draft dirty.
      instance.setValue(source);
      observed.current = readMarkdown(instance);
      selection.current = null;
    }
    if (type === "sv") instance.setPreviewMode(view === "split" ? "both" : "editor");
    for (const element of [instance.vditor.sv!.element, instance.vditor.ir!.element]) {
      element.removeAttribute("id");
      element.removeAttribute("aria-label");
      element.removeAttribute("aria-describedby");
      element.removeAttribute("aria-invalid");
    }
    const input = type === "sv" ? instance.vditor.sv!.element : instance.vditor.ir!.element;
    if (inputId) input.id = inputId;
    input.setAttribute("aria-label", "正文（Markdown）");
    input.setAttribute("role", "textbox");
    input.setAttribute("aria-multiline", "true");
    for (const key of ["aria-describedby", "aria-invalid", "aria-required"] as const) {
      const attribute = latest.current[key];
      if (attribute !== undefined) input.setAttribute(key, String(attribute));
    }
    if (readOnly) instance.disabled(); else instance.enable();
    const toolbar = instance.vditor.toolbar!.elements!;
    const media = toolbar["insert-image"];
    media.hidden = !latest.current.onOpenMedia;
    const mediaButton = media.querySelector<HTMLButtonElement>("button")!;
    mediaButton.disabled = readOnly || !latest.current.onOpenMedia;
    mediaButton.classList.toggle("vditor-menu--disabled", mediaButton.disabled);
    mediaButton.setAttribute("aria-expanded", String(!!latest.current.mediaOpen));
    mediaButton.setAttribute("aria-haspopup", "dialog");
    instance.vditor.element.querySelectorAll("button").forEach(button => { button.type = "button"; });
  }

  useEffect(() => {
    let disposed = false;
    let instance: Vditor | null = null;
    let unlink = () => {};
    setError(null);
    void loadVditor().then(Constructor => {
      if (disposed || !host.current) return;
      instance = new Constructor(host.current, {
        cdn: editorCdn, lang: "zh_CN", i18n: window.VditorI18n,
        mode: latest.current.effectiveMode === "ir" ? "ir" : "sv",
        value: latest.current.value, cache: { enable: false },
        height: "100%", minHeight: 360, undoDelay: 150,
        placeholder: "在此输入 Markdown 正文…",
        toolbar: ["headings", "bold", "italic", "strike", "link",
          {
            name: "insert-image", tip: "插入图片", tipPosition: "ne",
            icon: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 3h16a1 1 0 0 1 1 1v16a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1zm1 2v14h14V5H5zm2 12 4-5 3 3 2-2 2 4H7zm9-10a2 2 0 1 1 0 4 2 2 0 0 1 0-4z"/></svg>',
            click: () => {
              if (!latest.current.disabled) { publish(); latest.current.onOpenMedia?.(); }
            },
          },
          "list", "ordered-list", "check", "quote", "code", "inline-code", "table", "undo", "redo", "edit-mode"],
        toolbarConfig: { pin: false },
        preview: {
          delay: 120, mode: latest.current.effectiveMode === "split" ? "both" : "editor",
          maxWidth: 1920, actions: [],
          markdown: { sanitize: true, autoSpace: false, fixTermTypo: false },
          math: { engine: "KaTeX" },
          render: { media: { enable: false } },
        },
        link: { isOpen: false }, image: { isPreview: false },
        input: () => { if (!disposed) publish(); },
        blur: () => { if (!disposed) publish(); },
        after: () => {
          if (disposed || !instance) return;
          editor.current = instance;
          instance.setValue(latest.current.value, true);
          applied.current = latest.current.value;
          observed.current = readMarkdown(instance);
          if (latest.current.disabled) instance.disabled();
          unlink = linkEditorScroll(instance.vditor.sv!.element, instance.vditor.preview!.element,
            () => latest.current.effectiveMode === "split");
          setReady(true);
        },
      });
    }).catch(cause => { if (!disposed) setError(cause instanceof Error ? cause.message : "编辑器加载失败"); });
    return () => {
      disposed = true;
      unlink();
      editor.current = null;
      // Runtime resources were loaded before construction, so destroy is safe even during init.
      if (instance) {
        window.clearTimeout(instance.vditor.sv!.processTimeoutId);
        window.clearTimeout(instance.vditor.ir!.processTimeoutId);
        instance.destroy();
      }
    };
  }, [retry]);

  useLayoutEffect(() => {
    const instance = editor.current;
    if (!ready || !instance) return;
    const created = scope.current === "new" && applied.current === value;
    const changedTarget = scope.current !== editorScope && !created;
    scope.current = editorScope;
    if (applied.current !== value || changedTarget) {
      // External replacement (load/recovery) must not undo into another document.
      instance.setValue(value, true);
      applied.current = value;
      observed.current = readMarkdown(instance);
      selection.current = null;
    }
    configure(instance);
  }, [value, ready, effectiveMode, disabled, id, editorScope, props.onOpenMedia, props.mediaOpen, props["aria-describedby"], props["aria-invalid"]]);

  useEffect(() => {
    const rememberSelection = () => {
      const instance = editor.current;
      const range = window.getSelection()?.rangeCount ? window.getSelection()!.getRangeAt(0) : null;
      if (instance?.getCurrentMode() === "ir" && range && instance.vditor.ir!.element.contains(range.commonAncestorContainer)) {
        selection.current = range.cloneRange();
      }
    };
    document.addEventListener("selectionchange", rememberSelection);
    return () => document.removeEventListener("selectionchange", rememberSelection);
  }, []);

  useEffect(() => {
    contentRef({ insertImages(images: EditorImage[], replaceSelection: boolean) {
      const instance = editor.current;
      if (latest.current.disabled) return latest.current.value;
      if (instance?.getCurrentMode() === "ir") {
        const element = instance.vditor.ir!.element;
        const range = selection.current && element.contains(selection.current.commonAncestorContainer)
          ? selection.current.cloneRange() : document.createRange();
        if (!selection.current || !element.contains(range.commonAncestorContainer)) {
          range.selectNodeContents(element); range.collapse(false);
        }
        if (!replaceSelection) range.collapse(true);
        element.focus();
        window.getSelection()?.removeAllRanges(); window.getSelection()?.addRange(range);
        const markdown = images.map(image => insertImageMarkdown("", 0, 0, image.url, image.alt).value).join("\n\n");
        instance.insertMD(markdown);
        publish();
        return readMarkdown(instance);
      }
      const element = instance?.vditor.sv!.element ?? fallback.current;
      let next = latest.current.value;
      let start = element?.selectionStart ?? next.length;
      let end = replaceSelection ? (element?.selectionEnd ?? start) : start;
      for (const image of images) {
        const result = insertImageMarkdown(next, start, end, image.url, image.alt);
        next = result.value; start = result.selectionStart; end = start;
      }
      if (instance) { instance.setValue(next); observed.current = readMarkdown(instance); }
      applied.current = next;
      latest.current.onChange?.(next);
      requestAnimationFrame(() => {
        if (element?.isConnected) { element.focus(); element.setSelectionRange(start, start); }
      });
      return next;
    } });
    return () => contentRef(null);
  }, [contentRef]);

  function files(event: React.ClipboardEvent | React.DragEvent) {
    const list = Array.from("clipboardData" in event ? event.clipboardData.files : event.dataTransfer.files);
    if (!list.length) return;
    event.preventDefault(); event.stopPropagation();
    if (!latest.current.disabled) void latest.current.onInsertFiles?.(list);
  }

  return <div className="markdown-editor" data-mode={effectiveMode} style={{
    "--editor-border": token.colorBorderSecondary, "--editor-surface": token.colorBgContainer,
    borderRadius: token.borderRadiusLG,
  } as CSSProperties}>
    <Flex className="markdown-editor-toolbar" gap={8} wrap justify="space-between" align="center">
      <Segmented<Mode> size="small" aria-label="编辑器视图" value={effectiveMode} disabled={!ready || composing}
        options={[{ value: "ir", label: "即时渲染" }, ...(screens.md ? [{ value: "split" as const, label: "双栏" }] : []), { value: "source", label: "源码" }]}
        onChange={next => { publish(); setMode(next); }} />
    </Flex>
    {error && <Alert type="warning" showIcon title={error} action={<Button size="small" onClick={() => setRetry(n => n + 1)}>重试</Button>} />}
    <div className="markdown-editor-workspace" onPasteCapture={files} onDropCapture={files}
      onDragOver={event => { if (event.dataTransfer.types.includes("Files")) event.preventDefault(); }}
      onInput={() => publish()} onKeyUp={() => publish()} onClick={() => publish()}
      onCompositionStart={() => setComposing(true)} onCompositionEnd={() => { setComposing(false); publish(); }}
      onKeyDownCapture={event => {
        if ((event.ctrlKey || event.metaKey) && event.altKey && ["7", "8", "9"].includes(event.key)) {
          event.preventDefault(); event.stopPropagation();
          if (!composing) { publish(); setMode(event.key === "9" ? "split" : "ir"); }
        }
      }}>
      {!ready && <div className="markdown-editor-fallback">
        {!error && <Spin size="small" aria-label="加载编辑器" />}
        <Input.TextArea id={id} aria-label="正文（Markdown）" value={value} disabled={disabled}
          ref={node => { fallback.current = node?.resizableTextArea?.textArea ?? null; }}
          onChange={event => props.onChange?.(event.target.value)} placeholder="在此输入 Markdown 正文…" />
      </div>}
      <div ref={host} className="markdown-editor-vditor" hidden={!ready} />
    </div>
    <div className="markdown-editor-footer"><Typography.Text type="secondary">
      {effectiveMode === "split" ? "双栏滚动联动。" : ""}保存后才会修改内容。
    </Typography.Text></div>
  </div>;
}
