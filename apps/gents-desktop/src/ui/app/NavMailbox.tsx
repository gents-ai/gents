import { ChevronRight } from "lucide-react";
import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import { cn } from "@gents/ui/lib/utils";
import { href } from "@/lib/router";
import { listedSession } from "../../hooks/fleetStore";
import { useFleet } from "@/hooks/useFleet";
import { Hint } from "@/screens/Hint";
import { kindOf } from "@/screens/MailboxItem";
import { openInStack } from "@/screens/MailboxStack";
import type { TriageGroup } from "@/screens/mailbox-triage";
import { listViews } from "./listViews";
import { useNewItem } from "./mailboxSeen";

function MailboxLink({ item }: { item: MailboxItemView }) {
  const kind = kindOf(item);
  const Icon = kind.icon;
  const fresh = useNewItem(item);
  const session = useFleet((state) =>
    item.sessionId ? listedSession(state, item.nodeDid, item.sessionId) : null,
  );
  const from = item.sessionId ? session?.title?.trim() || "Session" : null;
  const label = kind.label + (fresh ? " · new since you looked" : "");
  return (
    <li>
      <Hint label={[label, item.title, from].filter(Boolean).join(" · ")}>
        <button
          type="button"
          onClick={() => openInStack(item)}
          className="flex h-8 w-full items-center gap-2.5 rounded-lg px-2 text-left text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        >
          {/* drawn as a session's status is, in the same slot, and marked
              new the way a session with an icon for its status is */}
          <span
            role="img"
            aria-label={label}
            className="relative grid size-4 shrink-0 place-items-center"
          >
            <Icon className={cn("size-3.5", kind.tone ?? "text-muted-foreground")} />
            {fresh && (
              <span className="absolute -top-0.5 -right-0.5 size-1.5 rounded-full bg-brand ring-2 ring-background" />
            )}
          </span>
          <span className="min-w-0 flex-1 truncate">{item.title}</span>
        </button>
      </Hint>
    </li>
  );
}

/** A side nav section for one mailbox category, under the Mailbox page's
    name for it, or for every open item: its first few, in the mailbox's
    order, each opening over the page showing now. The panel leaves out an
    empty one. */
export function NavMailbox({
  group,
  limit,
}: {
  group: Omit<TriageGroup, "key"> & { key: TriageGroup["key"] | "all" };
  limit: number;
}) {
  const found = group;
  const shown = group.items;
  const more = shown.length - limit;
  return (
    <>
      <p className="mt-2 mb-0 px-5 font-mono text-[10px] tracking-wide text-muted-foreground uppercase truncate">
        {found.label}
        {/* how many wait, of which the first few are listed */}
        <span className="tabular-nums"> · {shown.length}</span>
      </p>
      <ul className="px-3" aria-label={found.label}>
        {shown.slice(0, limit).map((item) => (
          <MailboxLink key={`${item.nodeDid}:${item.itemId}`} item={item} />
        ))}
      </ul>
      {more > 0 && (
        <a
          href={href({ name: "mailbox" })}
          onClick={() => listViews.setMailboxTab(group.key)}
          className="flex items-center gap-1 px-5 py-1 text-xs whitespace-nowrap text-muted-foreground hover:text-foreground"
        >
          <span className="min-w-0 truncate">{more} more in Mailbox</span>
          <ChevronRight className="size-3 shrink-0" />
        </a>
      )}
    </>
  );
}
