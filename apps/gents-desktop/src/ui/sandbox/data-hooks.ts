/* Reactive reads and writes over `ctx.data`, for a page. `useQuery` runs
   one declared query tool, holds loading/error/data, cancels a stale
   answer when its inputs change or the page unmounts, re-runs on the
   shell's `client-updated` events (debounced) and on a bounded interval
   while mounted. The tool answers one page of at most 1,000 rows and
   truncates a string past 2,000 bytes; the hook can follow both — the
   `next_offset` of a full page and the `field_recovery` of a cut string —
   so a page sees whole rows and whole values without writing either loop.
   The shell owns the change signal. */
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
  /** the tool's own arguments (`fields`, `limit`, `offset`, `order`, filter values…) */
  args?: Record<string, unknown>;
  /** re-run on `client-updated`; default true */
  live?: boolean;
  /** re-run every so many ms while mounted; default 30 000, 0 disables */
  intervalMs?: number;
  /** skip while false (a surface not chosen yet) */
  enabled?: boolean;
  /** `"one"` (default) answers the page the args ask for; `"all"` follows
      `next_offset` until a short page or `maxRows`, and the result carries
      every row with `count` summed and `next_offset` only when it stopped
      at the cap */
  pages?: "one" | "all";
  /** the most rows `pages: "all"` gathers; default 10 000 */
  maxRows?: number;
  /** follow `field_recovery` and splice the whole value back into its row;
      default true. A value that changes between its pages is left as
      answered, with the recovery entry kept. */
  recoverFields?: boolean;
}

const REFRESH_DEBOUNCE_MS = 300;
const DEFAULT_MAX_ROWS = 10_000;

interface QueryPage {
  results?: unknown[];
  count?: number;
  next_offset?: number;
  field_recovery?: FieldRecovery[];
  [key: string]: unknown;
}

interface FieldRecovery {
  row: number;
  doc_id: string;
  field: string;
  total_bytes: number;
  field_page: { doc_id: string; field: string; offset_bytes: number };
}

interface FieldPage {
  text: string;
  next_offset_bytes: number;
  value_hash: string;
  complete: boolean;
}

type Call = (args: Record<string, unknown>) => Promise<UxDataCallResult>;

/* the whole value, page after page, each one carrying the hash of the value
   the earlier pages came from; null when the value moved under us */
async function recoverField(
  call: Call,
  args: Record<string, unknown>,
  entry: FieldRecovery,
) {
  let offset = 0;
  let hash: string | undefined;
  let text = "";
  for (;;) {
    const pageArgs: Record<string, unknown> = {
      ...args,
      field_page: { doc_id: entry.doc_id, field: entry.field, offset_bytes: offset },
    };
    if (hash) (pageArgs.field_page as Record<string, unknown>).expected_hash = hash;
    let page: FieldPage;
    try {
      page = (
        (await call(pageArgs)).output as {
          field_page: FieldPage;
        }
      ).field_page;
    } catch {
      return null;
    }
    text += page.text;
    if (page.complete) return text;
    if (page.next_offset_bytes <= offset) return null;
    offset = page.next_offset_bytes;
    hash = page.value_hash;
  }
}

async function recoverFields(
  call: Call,
  args: Record<string, unknown>,
  page: QueryPage,
) {
  const entries = page.field_recovery ?? [];
  if (entries.length === 0 || !Array.isArray(page.results)) return page;
  const results = page.results.map((row) =>
    row && typeof row === "object" ? { ...(row as Record<string, unknown>) } : row,
  );
  const left: FieldRecovery[] = [];
  /* the recovered field must stay selected on the follow-up; the tool's
     projection otherwise matches the original call */
  const selected = Array.isArray(args.fields) ? (args.fields as string[]) : null;
  for (const entry of entries) {
    const followArgs =
      selected && !selected.includes(entry.field)
        ? { ...args, fields: [...selected, entry.field] }
        : args;
    const whole = await recoverField(call, followArgs, entry);
    const row = results[entry.row];
    if (whole === null || !row || typeof row !== "object") {
      left.push(entry);
      continue;
    }
    (row as Record<string, unknown>)[entry.field] = whole;
  }
  const out: QueryPage = { ...page, results };
  if (left.length === 0) delete out.field_recovery;
  else out.field_recovery = left;
  return out;
}

/* every page up to the cap: a full page is followed by its next_offset, a
   short one ends the walk; each page's long values are recovered before
   its rows join, and a recovery left over keeps its row index in the whole */
async function allPages(
  call: Call,
  args: Record<string, unknown>,
  maxRows: number,
  recover: boolean,
) {
  const first = (await call(args)).output as QueryPage;
  if (!Array.isArray(first.results)) return first;
  const results: unknown[] = [];
  const left: FieldRecovery[] = [];
  let page = first;
  for (;;) {
    if (recover) page = await recoverFields(call, args, page);
    for (const entry of page.field_recovery ?? []) {
      left.push({ ...entry, row: entry.row + results.length });
    }
    results.push(...page.results!);
    if (typeof page.next_offset !== "number" || results.length >= maxRows) break;
    const next = (await call({ ...args, offset: page.next_offset }))
      .output as QueryPage;
    if (!Array.isArray(next.results)) break;
    page = next;
  }
  const out: QueryPage = { ...first, results: results.slice(0, maxRows) };
  out.count = out.results!.length;
  delete out.next_offset;
  delete out.limit_note;
  delete out.field_recovery;
  if (typeof page.next_offset === "number" && results.length >= maxRows) {
    out.next_offset = page.next_offset - (results.length - maxRows);
    out.limit_note = `stopped at ${maxRows} rows; continue with offset ${out.next_offset} or narrow the filter`;
  }
  const kept = left.filter((entry) => entry.row < maxRows);
  if (kept.length > 0) out.field_recovery = kept;
  return out;
}

export function useQuery<T = unknown>(
  ctx: UxContext,
  surface: string,
  tool: string,
  options: UseQueryOptions = {},
): QueryState<T> {
  const {
    live = true,
    intervalMs = 30_000,
    enabled = true,
    pages = "one",
    maxRows = DEFAULT_MAX_ROWS,
    recoverFields: recover = true,
  } = options;
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
    const args = JSON.parse(argsKey) as Record<string, unknown>;
    const call: Call = (a) => ctx.data.call(surface, tool, a);
    (async () => {
      if (pages === "all") return allPages(call, args, maxRows, recover);
      const page = (await call(args)).output as QueryPage;
      return recover ? recoverFields(call, args, page) : page;
    })()
      .then((result) => {
        if (generation.current !== mine) return;
        setState({ data: result as T, error: null, loading: false });
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
  }, [ctx, surface, tool, argsKey, enabled, tick, pages, maxRows, recover]);

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
