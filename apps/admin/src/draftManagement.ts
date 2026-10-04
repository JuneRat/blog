import { deleteInactiveDraft, draftActivities, type DraftActivity, type DraftDeletion } from "./draftActivity";

export interface StoredDraft {
  key: string; raw: string; kind: "post" | "page"; id: string | null;
  writerId: string | null; title: string; savedAt: string | null; baselineVersion: number | null;
  value: Record<string, unknown> | null; bytes: number; activity: DraftActivity; coordinated: boolean;
}

/** Enumerate only the signed-in owner's namespaces; do not scan on every keystroke. */
export async function listStoredDrafts(owner: string): Promise<StoredDraft[]> {
  const roots = { current: `blog:local-draft:v2:${encodeURIComponent(owner)}:`, legacy: `blog:local-draft:v1:${encodeURIComponent(owner)}:` };
  const drafts: StoredDraft[] = [];
  for (let index = 0; index < localStorage.length; index += 1) {
    const key = localStorage.key(index);
    if (!key) continue;
    const legacy = key.startsWith(roots.legacy);
    if (!legacy && !key.startsWith(roots.current)) continue;
    const parts = key.slice((legacy ? roots.legacy : roots.current).length).split(":");
    if (parts.length !== (legacy ? 2 : 4) || !["post", "page"].includes(parts[0]!)) continue;
    const raw = localStorage.getItem(key);
    if (raw === null) continue;
    let id: string | null;
    try { id = parts[1] === "new" ? null : decodeURIComponent(parts[1]!); } catch { continue; }
    let value: Record<string, unknown> | null = null;
    let savedAt: string | null = null;
    let baselineVersion: number | null = null;
    let coordinated = false;
    try {
      const parsed: unknown = JSON.parse(raw);
      if (parsed && typeof parsed === "object") {
        const draft = parsed as Record<string, unknown>;
        const version = draft.baselineVersion;
        coordinated = !legacy && draft.schema === 2 && draft.coordination === "web-lock-v1";
        if ([1, 2].includes(draft.schema as number) && typeof draft.savedAt === "string" && Number.isFinite(Date.parse(draft.savedAt)) &&
          (version === null ? id === null : Number.isSafeInteger(version) && Number(version) > 0) &&
          draft.value && typeof draft.value === "object" && !Array.isArray(draft.value)) {
          savedAt = draft.savedAt; baselineVersion = version as number | null;
          value = draft.value as Record<string, unknown>;
        }
      }
    } catch { /* Broken copies remain visible for inspection instead of silently disappearing. */ }
    drafts.push({ key, raw, kind: parts[0] as "post" | "page", id, writerId: legacy ? null : parts[3]!,
      title: typeof value?.title === "string" && value.title.trim() ? value.title : "无标题副本",
      savedAt, baselineVersion, value, bytes: (key.length + raw.length) * 2, activity: "unknown", coordinated });
  }
  const activity = await draftActivities(drafts.filter(draft => draft.coordinated).map(draft => draft.key));
  return drafts.map(draft => ({ ...draft, activity: activity.get(draft.key) ?? "unknown" }))
    .sort((left, right) => (right.savedAt ? Date.parse(right.savedAt) : 0) - (left.savedAt ? Date.parse(left.savedAt) : 0));
}

export function removeStoredDraft(draft: StoredDraft): Promise<DraftDeletion> {
  // Legacy sources have no independent writer identity, so no lock can prove ownership.
  return !draft.coordinated ? Promise.resolve("unknown") : deleteInactiveDraft(draft.key, draft.raw);
}

export function draftSize(bytes: number): string {
  return bytes < 1024 ? `${bytes} B` : bytes < 1024 * 1024 ? `${(bytes / 1024).toFixed(1)} KiB` : `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
}
