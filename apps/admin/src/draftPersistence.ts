/** Continuous typing is persisted at most once per interval, without postponing forever. */
export const DRAFT_WRITE_INTERVAL_MS = 400;

export function createDraftWriteQueue() {
  let pending: (() => void) | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  function cancel() {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    pending = null;
  }
  function flush() {
    const write = pending;
    cancel();
    write?.();
  }
  return {
    enqueue(write: () => void) {
      pending = write;
      if (timer === null) timer = setTimeout(flush, DRAFT_WRITE_INTERVAL_MS);
    },
    cancel,
    flush,
  };
}

/** Compare form shapes without copying or serializing their potentially large body strings. */
export function draftValueEquals(left: unknown, right: unknown): boolean {
  if (left === right) return true;
  if (left === null || right === null || typeof left !== "object" || typeof right !== "object") return false;
  if (Array.isArray(left) !== Array.isArray(right)) return false;
  const keys = Object.keys(left);
  if (keys.length !== Object.keys(right).length) return false;
  return keys.every(key => Object.prototype.hasOwnProperty.call(right, key) &&
    draftValueEquals((left as Record<string, unknown>)[key], (right as Record<string, unknown>)[key]));
}
