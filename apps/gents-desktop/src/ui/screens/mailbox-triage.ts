/* How the mailbox orders what the agent filed. An inbox that lists items in
   arrival order makes a person read all of it to find the one thing that
   is waiting on them; this puts what needs a decision first, and within a
   group whatever is due soonest first. The words are the person's, not the
   schema's: "needs a decision" rather than "ask | gate". A deadline that
   has passed is not a state here: the runtime expires such an item on its
   next sweep, so the open list never holds one for long. */
import type { MailboxItemView } from "@source-inc/gents-desktop-client";

export type Triage = "decide" | "wrong" | "look" | "done";

export const TRIAGE: { key: Triage; label: string; kinds: string[] }[] = [
  /* the agent is stopped until a person answers */
  { key: "decide", label: "Needs a decision", kinds: ["ask", "gate"] },
  { key: "wrong", label: "Went wrong", kinds: ["failed"] },
  { key: "look", label: "Worth a look", kinds: ["flag"] },
  { key: "done", label: "Done", kinds: ["finished"] },
];

/* a kind this build does not know still lands somewhere a person looks */
export const triageOf = (kind: string): Triage =>
  TRIAGE.find((t) => t.kinds.includes(kind))?.key ?? "look";

export const deadlineOf = (item: MailboxItemView): number | null => {
  if (!item.deadlineAt) return null;
  const t = Date.parse(item.deadlineAt);
  return Number.isNaN(t) ? null : t;
};

/* the soonest deadline first, then whatever has a deadline before whatever
   does not, then newest first: the order a person would work through them */
export function orderItems(items: MailboxItemView[]): MailboxItemView[] {
  return [...items].sort((a, b) => {
    const ad = deadlineOf(a);
    const bd = deadlineOf(b);
    if (ad !== null && bd !== null && ad !== bd) return ad - bd;
    if ((ad === null) !== (bd === null)) return ad === null ? 1 : -1;
    return Date.parse(b.createdAt) - Date.parse(a.createdAt);
  });
}

/* a search is over what the person can see on the card, payload included:
   an item is often findable only by the build number or path inside it */
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

/* the groups in triage order, empty ones left out, each ordered within */
export function groupItems(items: MailboxItemView[]): TriageGroup[] {
  return TRIAGE.map((t) => ({
    key: t.key,
    label: t.label,
    items: orderItems(items.filter((m) => triageOf(m.kind) === t.key)),
  })).filter((g) => g.items.length > 0);
}

/* the kinds, in the order a person works through them */
export const KIND_ORDER = ["ask", "gate", "failed", "flag", "finished"];
