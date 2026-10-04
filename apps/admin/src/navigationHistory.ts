/**
 * Keep the rendered route unchanged while a browser history traversal is being
 * confirmed. Returning to the original entry uses go(), never pushState(): a
 * cancelled Back must preserve both the draft and the original Forward stack.
 */
export type HistoryBlocker = (decide: (leave: boolean) => void) => () => void;

interface Position {
  session: string;
  index: number;
}

interface Location {
  url: string;
  position: Position;
}

interface Traversal {
  target: Location;
  decision?: boolean;
  restoring: boolean;
  replaying: boolean;
  prompted: boolean;
  closePrompt?: () => void;
  replaceNavigation?: () => void;
}

const STATE_KEY = "blogAdminHistory";

function positionOf(state: unknown): Position | null {
  if (state === null || typeof state !== "object" || !(STATE_KEY in state)) return null;
  const value = state[STATE_KEY];
  if (value === null || typeof value !== "object" || !("session" in value) || !("index" in value)) return null;
  return typeof value.session === "string" && typeof value.index === "number" && Number.isSafeInteger(value.index)
    ? { session: value.session, index: value.index } : null;
}

function sameLocation(a: Location, b: Location): boolean {
  return a.url === b.url && a.position.session === b.position.session && a.position.index === b.position.index;
}

export class NavigationHistory {
  private current: Location;
  private blocker: HistoryBlocker | null = null;
  private pending: Traversal | null = null;
  private listeners = new Set<() => void>();

  constructor(private readonly browser: Window) {
    this.current = this.read();
    browser.addEventListener("popstate", this.onPop);
  }

  private read(): Location {
    let position = positionOf(this.browser.history.state);
    if (position === null) {
      // getRandomValues also works on plain HTTP development/LAN origins.
      position = { session: Array.from(this.browser.crypto.getRandomValues(new Uint32Array(4))).join("-"), index: 0 };
      this.browser.history.replaceState(this.state(position), "");
    }
    return { url: this.browser.location.href, position };
  }

  private state(position: Position): Record<string, unknown> {
    return { ...this.browser.history.state, [STATE_KEY]: position };
  }

  getURL = (): string => this.current.url;

  getPathname = (): string => new URL(this.current.url).pathname;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };

  private publish(location: Location): void {
    this.current = location;
    this.listeners.forEach(listener => listener());
  }

  block(blocker: HistoryBlocker): () => void {
    this.blocker = blocker;
    return () => {
      if (this.blocker !== blocker) return;
      this.blocker = null;
      if (this.pending && !this.pending.replaying) {
        this.pending.closePrompt?.();
        this.pending.decision = false;
        this.continueTraversal();
      }
    };
  }

  private atCurrent(): boolean {
    const position = positionOf(this.browser.history.state);
    return position !== null && sameLocation({ url: this.browser.location.href, position }, this.current);
  }

  private continueTraversal(): void {
    const pending = this.pending;
    if (!pending || pending.restoring || pending.replaying || !this.atCurrent()) return;
    if (pending.replaceNavigation) {
      this.pending = null;
      pending.replaceNavigation();
    } else if (pending.decision === false) {
      this.pending = null;
    } else if (pending.decision === true) {
      pending.replaying = true;
      this.browser.history.go(pending.target.position.index - this.current.position.index);
    } else if (!pending.prompted && this.blocker) {
      pending.prompted = true;
      pending.closePrompt = this.blocker(leave => {
        if (this.pending !== pending) return;
        pending.decision = leave;
        this.continueTraversal();
      });
    }
  }

  private onPop = (): void => {
    const next = this.read();
    if (this.pending) {
      const pending = this.pending;
      if (pending.replaying && sameLocation(next, pending.target)) {
        this.pending = null;
        this.publish(next);
      } else if (this.atCurrent()) {
        pending.restoring = false;
        this.continueTraversal();
      } else if (next.position.session === this.current.position.session) {
        // Extra Back/Forward clicks while the dialog is open do not unmount
        // the editor or create additional confirmation dialogs.
        const destination = pending.replaying ? pending.target : this.current;
        pending.restoring = !pending.replaying;
        this.browser.history.go(destination.position.index - next.position.index);
      }
      return;
    }
    const delta = this.current.position.index - next.position.index;
    if (this.blocker && next.url !== this.current.url && delta !== 0
        && next.position.session === this.current.position.session) {
      this.pending = { target: next, restoring: true, replaying: false, prompted: false };
      this.browser.history.go(delta);
      return;
    }
    this.publish(next);
  };

  navigate(to: string, replace = false): void {
    const url = new URL(to, this.current.url).href;
    if (url === this.current.url) return;
    const commit = (): void => {
      const position = { ...this.current.position, index: this.current.position.index + (replace ? 0 : 1) };
      this.browser.history[replace ? "replaceState" : "pushState"](this.state(position), "", url);
      this.publish({ url, position });
    };
    if (this.pending) {
      // A completed create/save may replace the route while confirmation is
      // pending. Wait for history restoration before committing that route.
      this.pending.closePrompt?.();
      this.pending.replaceNavigation = commit;
      this.pending.restoring ||= this.pending.replaying;
      this.pending.replaying = false;
      this.continueTraversal();
    } else {
      commit();
    }
  }

  dispose(): void {
    this.browser.removeEventListener("popstate", this.onPop);
    this.pending?.closePrompt?.();
    this.pending = null;
  }
}
