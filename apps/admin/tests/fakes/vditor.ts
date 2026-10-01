// jsdom cannot load Lute or lay out contenteditable. This boundary double exercises
// Form/draft/media lifecycles; Playwright uses the real production Vditor bundle.
export default class FakeVditor {
  vditor: {
    element: HTMLElement;
    sv: { element: HTMLTextAreaElement; processTimeoutId?: number };
    ir: { element: HTMLDivElement; processTimeoutId?: number };
    preview: { element: HTMLDivElement };
    toolbar: { elements: Record<string, HTMLElement> };
  };
  private mode: string;
  private destroyed = false;
  private options: IOptions;
  constructor(host: HTMLElement, options: IOptions) {
    this.options = options;
    this.mode = options.mode ?? "sv";
    const source = document.createElement("textarea");
    source.className = "vditor-sv";
    const ir = document.createElement("div");
    ir.className = "vditor-ir";
    const preview = document.createElement("div");
    preview.appendChild(document.createElement("div"));
    const toolbar = document.createElement("div");
    const modes = document.createElement("div");
    for (const mode of ["sv", "ir"]) {
      const button = document.createElement("button"); button.dataset.mode = mode;
      button.addEventListener("click", () => { this.mode = mode; this.display(); });
      modes.appendChild(button);
    }
    modes.hidden = true;
    const bold = document.createElement("button"); bold.dataset.type = "bold"; bold.textContent = "粗体";
    bold.addEventListener("click", () => {
      if (source.disabled) return;
      const { selectionStart: start, selectionEnd: end, value } = source;
      this.setValue(value.slice(0, start) + "**" + value.slice(start, end) + "**" + value.slice(end));
      options.input?.(this.getValue());
    });
    toolbar.append(bold, modes);
    const elements: Record<string, HTMLElement> = { "edit-mode": modes };
    for (const item of options.toolbar ?? []) {
      if (typeof item === "string") continue;
      const wrapper = document.createElement("div");
      const control = document.createElement("button");
      control.dataset.type = item.name;
      control.setAttribute("aria-label", item.tip ?? item.name);
      control.addEventListener("click", event => { item.click?.(event, this.vditor as unknown as IVditor); });
      wrapper.appendChild(control); toolbar.appendChild(wrapper); elements[item.name] = wrapper;
    }
    host.append(toolbar, source, ir, preview);
    this.vditor = { element: host, sv: { element: source }, ir: { element: ir }, preview: { element: preview }, toolbar: { elements } };
    source.addEventListener("change", () => options.input?.(source.value));
    source.addEventListener("input", () => options.input?.(source.value));
    this.setValue(options.value ?? "");
    this.display();
    queueMicrotask(() => { if (!this.destroyed) options.after?.(); });
  }
  private display() {
    this.vditor.sv.element.style.display = this.mode === "sv" ? "block" : "none";
    this.vditor.ir.element.style.display = this.mode === "ir" ? "block" : "none";
  }
  setValue(value: string) { this.vditor.sv.element.value = value; this.vditor.ir.element.textContent = value; }
  getValue() { return this.mode === "sv" ? this.vditor.sv.element.value : (this.vditor.ir.element.textContent ?? ""); }
  getCurrentMode() { return this.mode; }
  setPreviewMode(mode: string) { this.vditor.preview.element.style.display = mode === "both" ? "block" : "none"; }
  enable() { this.vditor.sv.element.disabled = false; this.vditor.ir.element.contentEditable = "true"; }
  disabled() { this.vditor.sv.element.disabled = true; this.vditor.ir.element.contentEditable = "false"; }
  insertMD(value: string) { this.setValue(this.getValue() + value); this.options.input?.(this.getValue()); }
  destroy() { this.destroyed = true; this.vditor.element.replaceChildren(); }
}
