/** A draft editor holds its own slot; cleanup is an optional, separately guarded operation. */
const prefix = "blog:draft-editor:v1:";
const localEditors = new Map<string, { users: number; held: boolean; release: () => void }>();
export type DraftActivity = "active" | "idle" | "unknown";

function locks(): LockManager | undefined {
  try { return navigator.locks; } catch { return undefined; }
}
const lockName = (key: string) => `${prefix}${key}`;

export function draftCoordination(key: string): "web-lock-v1" | undefined {
  return localEditors.get(key)?.held ? "web-lock-v1" : undefined;
}

/** Never make ordinary draft persistence depend on Web Locks being available. */
export function retainDraftEditor(key: string): () => void {
  const existing = localEditors.get(key);
  if (existing) {
    existing.users += 1;
  } else {
    const manager = locks();
    const controller = new AbortController();
    let finish = () => {};
    const lifetime = new Promise<void>(resolve => { finish = resolve; });
    const entry = { users: 1, held: false, release: () => { entry.held = false; finish(); controller.abort(); } };
    localEditors.set(key, entry);
    if (manager) {
      void manager.request(lockName(key), { mode: "exclusive", signal: controller.signal }, async () => {
        if (controller.signal.aborted) return;
        entry.held = true;
        // A migration or immediate restore may have written before lock acquisition.
        // Only our own held slot can be upgraded; no revision or timestamp is changed.
        try {
          const raw = localStorage.getItem(key);
          const stored: unknown = raw === null ? null : JSON.parse(raw);
          if (stored && typeof stored === "object" && (stored as { schema?: unknown }).schema === 2) {
            localStorage.setItem(key, JSON.stringify({ ...stored, coordination: "web-lock-v1" }));
          }
        } catch { /* Metadata is optional; recovery must remain usable if storage is full. */ }
        await lifetime;
        entry.held = false;
      })
        .catch(() => { /* Unsupported or denied coordination must not block editing. */ });
    }
  }
  let released = false;
  return () => {
    if (released) return;
    released = true;
    const entry = localEditors.get(key);
    if (!entry || --entry.users > 0) return;
    localEditors.delete(key);
    entry.release();
  };
}

export async function draftActivities(keys: readonly string[]): Promise<Map<string, DraftActivity>> {
  const manager = locks();
  let occupied: Set<string> | null = null;
  if (manager) {
    try {
      const state = await manager.query();
      occupied = new Set([...state.held ?? [], ...state.pending ?? []].map(lock => lock.name!));
    } catch { /* Absence of evidence is not permission to remove a source slot. */ }
  }
  return new Map(keys.map(key => [key, localEditors.has(key) || occupied?.has(lockName(key)) ? "active" : occupied ? "idle" : "unknown"]));
}

export type DraftDeletion = "deleted" | "changed" | "missing" | "active" | "unknown";
/** Revalidate the exact source while owning the same lock used by its editor. */
export async function deleteInactiveDraft(key: string, raw: string): Promise<DraftDeletion> {
  if (localEditors.has(key)) return "active";
  const manager = locks();
  if (!manager) return "unknown";
  try {
    return await manager.request(lockName(key), { mode: "exclusive", ifAvailable: true }, lock => {
      if (!lock || localEditors.has(key)) return "active";
      const current = localStorage.getItem(key);
      if (current === null) return "missing";
      if (current !== raw) return "changed";
      localStorage.removeItem(key);
      return "deleted";
    });
  } catch { return "unknown"; }
}
