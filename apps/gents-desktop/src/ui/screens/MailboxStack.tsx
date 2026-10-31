import { useCallback, useEffect } from "react";
import { toast } from "sonner";
import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import { currentRoute, pathFor, type Route } from "@/lib/router";
import type { MailboxOpen } from "@/app/listViews";
import { defaultScope, mailboxInScope } from "@/lib/scope";
import { useInScope, useSelectedNodeDid } from "@/hooks/useClient";
import { listViews, useListViews } from "@/app/listViews";
import { groupItems, matches, triageOf, type TriageGroup } from "./mailbox-triage";
import { ItemDialog } from "./MailboxItem";
import { useMinute } from "./time";

const SCOPE = defaultScope("mailbox");

/** The active node's open mailbox items: those matching `query`, grouped,
    and the ones the selected tab shows, in the order the list draws them. */
export function useMailboxList(query: string): {
  items: MailboxItemView[];
  searched: MailboxItemView[];
  groups: TriageGroup[];
  shown: MailboxItemView[];
} {
  const items = useInScope((ctx) => mailboxInScope(SCOPE, ctx));
  const tab = useListViews((views) => views.mailboxTab);
  const searched = items.filter((m) => matches(m, query));
  const groups = groupItems(searched);
  const shown =
    tab === "all"
      ? groups.flatMap((g) => g.items)
      : (groups.find((g) => g.key === tab)?.items ?? []);
  return { items, searched, groups, shown };
}

export const MAILBOX = pathFor({ name: "mailbox" });

/** Opens `item` over the page showing now, in the tab its kind belongs to,
    so moving on goes through items like it. */
export function openInStack(item: MailboxItemView) {
  listViews.setMailboxTab(triageOf(item.kind));
  listViews.setMailboxOpen({
    nodeDid: item.nodeDid,
    itemId: item.itemId,
    at: 0,
    where: pathFor(currentRoute() ?? { name: "mailbox" }),
  });
}

/** The item open over the page at `where` on the active node, if any. */
export const openHere = (
  kept: MailboxOpen | null,
  nodeDid: string | null,
  where: string,
) => (kept && kept.nodeDid === nodeDid && kept.where === where ? kept.itemId : null);

/** The mailbox's item dialog, over whatever screen is showing. The mailbox's
    rows open it, and ⌘⇧M opens it anywhere at the first item; pressed again
    it closes. An item left open to look at a session is open again on
    coming back. On the mailbox it steps through
    the rows the search leaves; elsewhere through the whole tab. */
export function MailboxStack({ route }: { route: Route }) {
  const selectedNodeDid = useSelectedNodeDid();
  const now = useMinute();
  const query = useListViews((views) => views.mailboxQuery);
  const { shown } = useMailboxList(route.name === "mailbox" ? (query ?? "") : "");
  const kept = useListViews((views) => views.mailboxOpen);
  const where = pathFor(route);
  const openId = openHere(kept, selectedNodeDid, where);
  const open = useCallback(
    (itemId: string, at: number) =>
      listViews.setMailboxOpen({ nodeDid: selectedNodeDid, itemId, at, where }),
    [selectedNodeDid, where],
  );
  const close = useCallback(() => listViews.setMailboxOpen(null), []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || !e.shiftKey || e.altKey || e.code !== "KeyM")
        return;
      e.preventDefault();
      if (openId !== null) return close();
      const first = shown[0];
      if (first) open(first.itemId, 0);
      else toast("Nothing needs you right now", { id: "mailbox-empty" });
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [openId, shown, open, close]);
  return (
    <ItemDialog
      items={shown}
      openId={openId}
      lastAt={kept?.at ?? 0}
      now={now}
      onOpen={open}
      onClose={close}
    />
  );
}
