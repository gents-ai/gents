/* Reactive reads and writes over `ctx.data`, for a page. `useQuery` runs
   one declared query tool, holds loading/error/data, cancels a stale
   answer when its inputs change or the page unmounts, re-runs on the
   shell's `client-updated` events (debounced) and on a bounded interval
   while mounted, and pages with the tool's own `offset`/`limit`. The
   plugin writes no polling and no refetch bookkeeping; the shell owns the
   change signal. */
import { useCallback, useEffect, useRef, useState } from "react";

import type { UxContext, UxDataCallResult } from "./contract";

export interface QueryState<T = unknown> {
  data: T | null;
  error: string | null;
  loading: boolean;
  /** run again now */
  refresh: () => void;
}

export interface UseQueryOptions {
  /** the tool's own arguments (`fields`, `limit`, filter values…) */
  args?: Record<string, unknown>;
  /** re-run on `client-updated`; default true */
  live?: boolean;
  /** re-run every so many ms while mounted; default 30 000, 0 disables */
  intervalMs?: number;
  /** skip while false (a surface not chosen yet) */
  enabled?: boolean;
}

const REFRESH_DEBOUNCE_MS = 300;

export function useQuery<T = unknown>(
  ctx: UxContext,
  surface: string,
  tool: string,
  options: UseQueryOptions = {},
): QueryState<T> {
  const { live = true, intervalMs = 30_000, enabled = true } = options;
  const argsKey = JSON.stringify(options.args ?? {});
  const [state, setState] = useState<{
    data: T | null;
    error: string | null;
    loading: boolean;
  }>({
    data: null,
    error: null,
    loading: enabled,
  });
  const [tick, setTick] = useState(0);
  const generation = useRef(0);

  const refresh = useCallback(() => setTick((n) => n + 1), []);

  useEffect(() => {
    if (!enabled) return;
    const mine = ++generation.current;
    setState((s) => ({ ...s, loading: true }));
    ctx.data
      .call(surface, tool, JSON.parse(argsKey) as Record<string, unknown>)
      .then((result: UxDataCallResult) => {
        if (generation.current !== mine) return;
        setState({ data: result.output as T, error: null, loading: false });
      })
      .catch((error: unknown) => {
        if (generation.current !== mine) return;
        setState((s) => ({
          data: s.data,
          error: error instanceof Error ? error.message : String(error),
          loading: false,
        }));
      });
    return () => {
      /* a later run or the unmount makes this answer stale */
      if (generation.current === mine) generation.current++;
    };
  }, [ctx, surface, tool, argsKey, enabled, tick]);

  useEffect(() => {
    if (!enabled || !live) return;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const off = ctx.onEvent("client-updated", () => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(refresh, REFRESH_DEBOUNCE_MS);
    });
    return () => {
      off();
      if (timer) clearTimeout(timer);
    };
  }, [ctx, enabled, live, refresh]);

  useEffect(() => {
    if (!enabled || intervalMs <= 0) return;
    const timer = setInterval(refresh, intervalMs);
    return () => clearInterval(timer);
  }, [enabled, intervalMs, refresh]);

  return { ...state, refresh };
}

export interface WriteState {
  /** run one create tool; resolves with the receipt, rejects with the refusal */
  write: (args: Record<string, unknown>) => Promise<UxDataCallResult>;
  pending: boolean;
  error: string | null;
}

export function useWrite(ctx: UxContext, surface: string, tool: string): WriteState {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const write = useCallback(
    async (args: Record<string, unknown>) => {
      setPending(true);
      setError(null);
      try {
        return await ctx.data.call(surface, tool, args);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
        throw e;
      } finally {
        setPending(false);
      }
    },
    [ctx, surface, tool],
  );
  return { write, pending, error };
}
