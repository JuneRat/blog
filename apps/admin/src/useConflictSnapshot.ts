import { useEffect, useState } from "react";
import { permissionMessageOf } from "./apiError";

export function useConflictSnapshot<T>({ active, id, load }: { active: boolean; id: string | null; load: (id: string) => Promise<T> }) {
  const [revision, setRevision] = useState(0);
  const [state, setState] = useState<{ id: string; value: T | null; error: string | null; loading: boolean } | null>(null);
  useEffect(() => {
    if (!active || id === null) { setState(null); return; }
    let cancelled = false;
    setState({ id, value: null, error: null, loading: true });
    void load(id).then(value => {
      if (!cancelled) setState({ id, value, error: null, loading: false });
    }, error => {
      if (!cancelled) setState({ id, value: null, error: permissionMessageOf(error), loading: false });
    });
    return () => { cancelled = true; };
  }, [active, id, load, revision]);
  const current = active && state?.id === id ? state : null;
  return {
    snapshot: current?.value ?? null,
    error: current?.error ?? null,
    loading: current?.loading ?? active,
    refresh: () => { setState(null); setRevision(value => value + 1); },
  };
}
