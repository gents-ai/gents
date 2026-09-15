import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";

/* A reviewable draft. Nothing crosses the bridge until the user explicitly
   saves; reset restores the last bridge-confirmed value. */
export function useDraft<T extends object>(
  saved: T,
  persist: (next: T) => Promise<unknown>,
) {
  const [draft, setDraft] = useState<T>(saved);
  const [baseline, setBaseline] = useState<T>(saved);
  const baselineRef = useRef<T>(saved);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const savedKey = JSON.stringify(saved);
  useEffect(() => {
    const previousKey = JSON.stringify(baselineRef.current);
    setDraft((currentDraft) =>
      JSON.stringify(currentDraft) === previousKey ? saved : currentDraft,
    );
    baselineRef.current = saved;
    setBaseline(saved);
    // Object identity changes on every projected snapshot; semantic content is
    // the bridge-confirmed generation this draft needs to observe.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [savedKey]);
  const dirty = JSON.stringify(draft) !== JSON.stringify(baseline);
  const save = async () => {
    if (!dirty || saving) return;
    setSaving(true);
    setError(null);
    try {
      await persist(draft);
      baselineRef.current = draft;
      setBaseline(draft);
      toast("Saved");
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      setError(message);
      toast(`Save failed: ${message}`);
    } finally {
      setSaving(false);
    }
  };
  const set = <K extends keyof T>(k: K, v: T[K]) => {
    setError(null);
    setDraft((d) => ({ ...d, [k]: v }));
  };
  const choose = <K extends keyof T>(k: K, v: T[K]) => {
    set(k, v);
  };
  // Kept as the field blur callback so existing editors do not accidentally
  // submit. Save is intentionally owned by the explicit action below.
  const commit = () => undefined;
  const reset = () => {
    setDraft(baseline);
    setError(null);
  };
  const onEnter = (e: React.KeyboardEvent<HTMLElement>) => {
    if (e.key === "Enter" && !e.shiftKey) (e.target as HTMLElement).blur();
  };
  return {
    draft,
    set,
    choose,
    commit,
    onEnter,
    dirty,
    saving,
    error,
    save,
    reset,
  };
}

/* one item per line; the desktop app splits on newline or comma */
export const toLines = (items: string[]) => items.join("\n");
export const fromLines = (text: string) =>
  text
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter(Boolean);
export const fromLinesOrNull = (text: string) => {
  const items = fromLines(text);
  return items.length ? items : null;
};

/* the desktop app's number hints */
export const intOrNull = (s: string) =>
  s.trim() === "" ? null : Number.parseInt(s, 10);
export const floatOrNull = (s: string) =>
  s.trim() === "" ? null : Number.parseFloat(s);
export const str = (n: number | null | undefined) => (n == null ? "" : String(n));

export function optionalInteger(
  label: string,
  value: string,
  bounds: { min?: number; max?: number } = {},
) {
  const text = value.trim();
  if (!text) return null;
  if (!/^-?\d+$/.test(text)) throw new Error(`${label} must be a whole number`);
  const parsed = Number(text);
  if (!Number.isSafeInteger(parsed)) throw new Error(`${label} is out of range`);
  if (bounds.min != null && parsed < bounds.min)
    throw new Error(`${label} must be ${bounds.min} or more`);
  if (bounds.max != null && parsed > bounds.max)
    throw new Error(`${label} must be ${bounds.max} or less`);
  return parsed;
}

export function optionalNumber(
  label: string,
  value: string,
  bounds: { min?: number; max?: number } = {},
) {
  const text = value.trim();
  if (!text) return null;
  const parsed = Number(text);
  if (!Number.isFinite(parsed)) throw new Error(`${label} must be a number`);
  if (bounds.min != null && parsed < bounds.min)
    throw new Error(`${label} must be ${bounds.min} or more`);
  if (bounds.max != null && parsed > bounds.max)
    throw new Error(`${label} must be ${bounds.max} or less`);
  return parsed;
}

export function requiredHttpUrl(label: string, value: string) {
  const text = value.trim();
  if (!text) throw new Error(`${label} is required`);
  let parsed: URL;
  try {
    parsed = new URL(text);
  } catch {
    throw new Error(`${label} must be a valid URL`);
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:")
    throw new Error(`${label} must use http or https`);
  return text.replace(/\/$/, "");
}

export function optionalAbsolutePath(label: string, value: string) {
  const text = value.trim();
  if (!text) return null;
  if (!text.startsWith("/") && !/^[A-Za-z]:[\\/]/.test(text))
    throw new Error(`${label} must be an absolute path`);
  return text;
}

const GRAPHQL_NAME = /^[_A-Za-z][_0-9A-Za-z]*$/;

export function requiredGraphqlName(label: string, value: string) {
  const text = value.trim();
  if (!text) throw new Error(`${label} is required`);
  if (!GRAPHQL_NAME.test(text)) throw new Error(`${label} must be a GraphQL name`);
  return text;
}

export function requiredGraphqlCollection(label: string, value: string) {
  const text = requiredGraphqlName(label, value);
  if (text.startsWith("__"))
    throw new Error(`${label} cannot use the reserved __ prefix`);
  return text;
}

export function optionalGraphqlFilter(label: string, value: string) {
  const text = value.trim();
  if (!text) return null;
  if (!text.startsWith("{")) throw new Error(`${label} must be an object literal`);
  const stack: string[] = [];
  let inString = false;
  let escaped = false;
  let closed = false;
  for (const character of text) {
    if (inString) {
      if (escaped) escaped = false;
      else if (character === "\\") escaped = true;
      else if (character === '"') inString = false;
      continue;
    }
    if (closed && !/\s/.test(character))
      throw new Error(`${label} has trailing content`);
    if (character === '"') inString = true;
    else if (character === "{" || character === "[") stack.push(character);
    else if (character === "}" || character === "]") {
      const opening = stack.pop();
      if (
        !opening ||
        (character === "}" && opening !== "{") ||
        (character === "]" && opening !== "[")
      )
        throw new Error(`${label} is unbalanced`);
      if (!stack.length) closed = true;
    } else if (!/[:,A-Za-z0-9_+\.\-\s]/.test(character)) {
      throw new Error(`${label} contains an unsupported character`);
    }
  }
  if (inString || stack.length || !closed) throw new Error(`${label} is unclosed`);
  return text;
}

const CRON_RANGES = [
  [0, 59],
  [0, 23],
  [1, 31],
  [1, 12],
  [0, 7],
] as const;
const CRON_NAMES: Record<number, Record<string, number>> = {
  3: Object.fromEntries(
    [
      "JAN",
      "FEB",
      "MAR",
      "APR",
      "MAY",
      "JUN",
      "JUL",
      "AUG",
      "SEP",
      "OCT",
      "NOV",
      "DEC",
    ].map((name, index) => [name, index + 1]),
  ),
  4: Object.fromEntries(
    ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"].map((name, index) => [
      name,
      index,
    ]),
  ),
};

function cronValue(field: number, value: string) {
  const named = CRON_NAMES[field]?.[value.toUpperCase()];
  const parsed = named ?? (/^\d+$/.test(value) ? Number(value) : Number.NaN);
  const [minimum, maximum] = CRON_RANGES[field];
  if (!Number.isInteger(parsed) || parsed < minimum || parsed > maximum)
    throw new Error(`Cron field ${field + 1} has an invalid value: ${value}`);
  return parsed;
}

export function validateCronSchedule(expression: string, timezone: string) {
  const text = expression.trim();
  const fields = text.split(/\s+/);
  if (fields.length !== 5)
    throw new Error("Cron expression must contain exactly 5 fields");
  fields.forEach((raw, field) => {
    for (const part of raw.split(",")) {
      if (!part) throw new Error(`Cron field ${field + 1} contains an empty item`);
      const [base, step, extra] = part.split("/");
      if (extra != null || (step != null && (!/^\d+$/.test(step) || Number(step) < 1)))
        throw new Error(`Cron field ${field + 1} has an invalid step`);
      if (base === "*") continue;
      const range = base.split("-");
      if (range.length > 2)
        throw new Error(`Cron field ${field + 1} has an invalid range`);
      const start = cronValue(field, range[0]);
      const end = range.length === 2 ? cronValue(field, range[1]) : start;
      if (start > end)
        throw new Error(`Cron field ${field + 1} has a descending range`);
    }
  });
  const zone = timezone.trim();
  if (!zone) throw new Error("Timezone is required");
  try {
    new Intl.DateTimeFormat("en-US", { timeZone: zone }).format();
  } catch {
    throw new Error("Timezone must be a valid IANA timezone");
  }
  return { expression: text, timezone: zone };
}

/* ids for new documents, minted in the handler, never in render */
export const newId = (prefix: string) => `${prefix}-${Date.now().toString(36)}`;
