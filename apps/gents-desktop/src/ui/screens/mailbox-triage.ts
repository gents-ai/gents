import type { MailboxItemView } from "@source-inc/gents-desktop-client";

export type Triage = "decide" | "wrong" | "look" | "done";

export const TRIAGE: { key: Triage; label: string; kinds: string[] }[] = [
  { key: "decide", label: "Needs a decision", kinds: ["ask", "gate"] },
  { key: "wrong", label: "Went wrong", kinds: ["failed"] },
  { key: "look", label: "Worth a look", kinds: ["flag"] },
  { key: "done", label: "Done", kinds: ["finished"] },
];

export const triageOf = (kind: string): Triage =>
  TRIAGE.find((t) => t.kinds.includes(kind))?.key ?? "look";

export const deadlineOf = (item: MailboxItemView): number | null => {
  if (!item.deadlineAt) return null;
  const t = Date.parse(item.deadlineAt);
  return Number.isNaN(t) ? null : t;
};

export const dueSoon = (deadline: number | null, now: number) =>
  deadline !== null && deadline - now < 60 * 60_000;

export function orderItems(items: MailboxItemView[]): MailboxItemView[] {
  return [...items].sort((a, b) => {
    const ad = deadlineOf(a);
    const bd = deadlineOf(b);
    if (ad !== null && bd !== null && ad !== bd) return ad - bd;
    if ((ad === null) !== (bd === null)) return ad === null ? 1 : -1;
    return Date.parse(b.createdAt) - Date.parse(a.createdAt);
  });
}

export function matches(item: MailboxItemView, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [item.title, item.summary, item.payload, item.sourceId]
    .filter((s): s is string => Boolean(s))
    .some((s) => s.toLowerCase().includes(q));
}

export type TriageGroup = {
  key: Triage;
  label: string;
  items: MailboxItemView[];
};

export function groupItems(items: MailboxItemView[]): TriageGroup[] {
  return TRIAGE.map((t) => ({
    key: t.key,
    label: t.label,
    items: orderItems(items.filter((m) => triageOf(m.kind) === t.key)),
  })).filter((g) => g.items.length > 0);
}

export const KIND_ORDER = ["ask", "gate", "failed", "flag", "finished"];
