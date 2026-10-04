import { useEffect, useRef } from "react";

/** Requests belong to one mounted account/editor route, including a particular new-draft visit. */
export function useEditorRequestGuard(id: string | null) {
  const scope = useRef({ id });
  const mounted = useRef(true);
  if (scope.current.id !== id) scope.current = { id };
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  return () => {
    const started = scope.current;
    return () => mounted.current && scope.current === started;
  };
}
