/** Bidirectional progress sync, with an echo guard for programmatic scroll events. */
export function linkEditorScroll(source: HTMLElement, preview: HTMLElement, active: () => boolean) {
  let leader = source;
  const pending = new WeakMap<HTMLElement, number>();
  const range = (element: HTMLElement) => Math.max(0, element.scrollHeight - element.clientHeight);
  function sync(from: HTMLElement, to: HTMLElement) {
    const progress = range(from) ? Math.max(0, Math.min(1, from.scrollTop / range(from))) : 0;
    const top = progress * range(to);
    if (Math.abs(to.scrollTop - top) < 1) return;
    to.scrollTop = top;
    pending.set(to, to.scrollTop);
  }
  const onScroll = (event: Event) => {
    if (!active()) return;
    // Replace Vditor's one-way listener so the two algorithms cannot fight.
    event.stopImmediatePropagation();
    const from = event.currentTarget as HTMLElement;
    const expected = pending.get(from);
    pending.delete(from);
    if (expected !== undefined && Math.abs(from.scrollTop - expected) < 1) return;
    leader = from;
    sync(from, from === source ? preview : source);
  };
  source.addEventListener("scroll", onScroll, true);
  preview.addEventListener("scroll", onScroll, true);
  // Images, formulas and diagrams can change preview height after initial rendering.
  const resize = new ResizeObserver(() => {
    if (active()) sync(leader, leader === source ? preview : source);
  });
  resize.observe(source);
  if (preview.firstElementChild) resize.observe(preview.firstElementChild);
  return () => {
    source.removeEventListener("scroll", onScroll, true);
    preview.removeEventListener("scroll", onScroll, true);
    resize.disconnect();
  };
}
