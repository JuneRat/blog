/** Browser-only recovery data. No slot is shared by two active documents. */
export interface DraftIdentity { tabId: string; writerId: string }
export interface DraftSnapshot<T> {
  schema: 1 | 2;
  revision?: string;
  /** Set only while this document holds the slot's Web Lock. Older writers never declared it. */
  coordination?: "web-lock-v1";
  savedAt: string;
  baselineVersion: number | null;
  value: T;
}
export interface DraftCandidate<T> {
  key: string;
  snapshot: DraftSnapshot<T>;
  origin: "current" | "tab" | "other" | "legacy";
}
function draftToken(): string {
  // getRandomValues also works on HTTP LAN origins where randomUUID is unavailable.
  return Array.from(crypto.getRandomValues(new Uint32Array(4)), value => value.toString(16).padStart(8, "0")).join("");
}
let documentIdentity: DraftIdentity | undefined;
export function draftIdentity(): DraftIdentity {
  if (documentIdentity) return documentIdentity;
  let tabId = draftToken();
  try {
    const previous = sessionStorage.getItem("blog:draft-tab:v2");
    if (previous) tabId = previous;
    else sessionStorage.setItem("blog:draft-tab:v2", tabId);
  } catch { /* Recovery remains available through discovery if session storage is disabled. */ }
  // A duplicated tab can inherit sessionStorage. A fresh writer ID still gives it its own slot.
  documentIdentity = { tabId, writerId: draftToken() };
  return documentIdentity;
}
export function draftScope(owner: string, kind: string, id: string | null) {
  return `${encodeURIComponent(owner)}:${kind}:${id === null ? "new" : encodeURIComponent(id)}`;
}
export function draftKey(scope: string, identity: DraftIdentity) {
  return `blog:local-draft:v2:${scope}:${identity.tabId}:${identity.writerId}`;
}
export function snapshot<T>(value: T, baselineVersion: number | null): DraftSnapshot<T> {
  return { schema: 2, revision: draftToken(), savedAt: new Date().toISOString(), baselineVersion, value };
}
function validShape(value: unknown, template: unknown): boolean {
  if (template === null) return value === null || typeof value === "string";
  if (Array.isArray(template)) return Array.isArray(value) && value.every(item => typeof item === "string");
  if (typeof template !== "object") return typeof value === typeof template;
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const expected = Object.entries(template as object);
  if (expected.length === 0) return Object.values(value).every(item => typeof item === "string");
  return expected.every(([key, field]) => validShape((value as Record<string, unknown>)[key], field));
}
export function readCandidates<T>(scope: string, identity: DraftIdentity, template: T, newDraft: boolean): DraftCandidate<T>[] {
  const prefix = `blog:local-draft:v2:${scope}:`;
  const legacyKey = `blog:local-draft:v1:${scope}`;
  const ownKey = draftKey(scope, identity);
  const candidates: DraftCandidate<T>[] = [];
  for (let index = 0; index < localStorage.length; index += 1) {
    const key = localStorage.key(index);
    if (!key || !(key.startsWith(prefix) || key === legacyKey)) continue;
    try {
      const parsed = JSON.parse(localStorage.getItem(key)!) as DraftSnapshot<T>;
      if (![1, 2].includes(parsed.schema) || !Number.isFinite(Date.parse(parsed.savedAt)) ||
        !(parsed.baselineVersion === null ? newDraft : Number.isSafeInteger(parsed.baselineVersion) && parsed.baselineVersion > 0) ||
        !validShape(parsed.value, template)) continue;
      candidates.push({ key, snapshot: parsed, origin: key === ownKey ? "current" : key === legacyKey ? "legacy" : key.startsWith(`${prefix}${identity.tabId}:`) ? "tab" : "other" });
    } catch { /* A broken foreign slot must not stop this document from preserving its own edits. */ }
  }
  return candidates.sort((left, right) => {
    const preference = (item: DraftCandidate<T>) => item.origin === "current" ? 0 : item.origin === "tab" ? 1 : 2;
    return preference(left) - preference(right) || Date.parse(right.snapshot.savedAt) - Date.parse(left.snapshot.savedAt);
  });
}
export function candidateRevision(candidate: DraftCandidate<unknown>): string {
  if (candidate.snapshot.revision) return candidate.snapshot.revision;
  // Compact compatibility fingerprint for v1 snapshots without revision IDs.
  const text = JSON.stringify(candidate.snapshot);
  let hash = 2166136261;
  for (let index = 0; index < text.length; index += 1) hash = Math.imul(hash ^ text.charCodeAt(index), 16777619);
  return `${candidate.snapshot.savedAt}:${text.length}:${hash >>> 0}`;
}
export function handledKey(scope: string, identity: DraftIdentity) {
  return `blog:draft-handled:v2:${identity.tabId}:${scope}`;
}
export function readHandled(scope: string, identity: DraftIdentity): Record<string, string> {
  try {
    const parsed: unknown = JSON.parse(sessionStorage.getItem(handledKey(scope, identity)) ?? "{}");
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed) && Object.values(parsed).every(value => typeof value === "string")) return parsed as Record<string, string>;
  } catch { /* In-memory decisions still work when session storage is unavailable. */ }
  return {};
}
