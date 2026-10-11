import { useCallback, useEffect, useRef, useState } from "react";
import { call } from "./api";

export interface Loaded<T> { data: T | null; error: string | null; loading: boolean; reload: () => void }

/** Load a command's result; reload when `deps` change. `pollMs` re-fetches quietly. */
export function useLoad<T = any>(cmd: string | null, args: Record<string, unknown> = {}, deps: unknown[] = [], pollMs = 0): Loaded<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [tick, setTick] = useState(0);
  const seq = useRef(0);
  const key = JSON.stringify([cmd, args, ...deps]);
  useEffect(() => {
    setData(null);
  }, [key]);
  useEffect(() => {
    if (!cmd) { setData(null); return; }
    const mine = ++seq.current;
    setLoading(true);
    call<T>(cmd, args)
      .then((d) => { if (mine === seq.current) { setData(d); setError(null); } })
      .catch((e) => { if (mine === seq.current) setError(String(e.message ?? e)); })
      .finally(() => { if (mine === seq.current) setLoading(false); });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, tick]);
  useEffect(() => {
    if (!pollMs || !cmd) return;
    const t = setInterval(() => setTick((x) => x + 1), pollMs);
    return () => clearInterval(t);
  }, [pollMs, cmd]);
  const reload = useCallback(() => setTick((x) => x + 1), []);
  return { data, error, loading, reload };
}

/** Hash route: "#/results/12" → ["results", "12"]. */
export function useRoute(): [string[], (path: string) => void] {
  const read = () => window.location.hash.replace(/^#\/?/, "").split("/").filter(Boolean).map(decodeURIComponent);
  const [parts, setParts] = useState<string[]>(read);
  useEffect(() => {
    const on = () => setParts(read());
    window.addEventListener("hashchange", on);
    return () => window.removeEventListener("hashchange", on);
  }, []);
  const go = useCallback((path: string) => { window.location.hash = path.startsWith("/") ? path : `/${path}`; }, []);
  return [parts, go];
}
