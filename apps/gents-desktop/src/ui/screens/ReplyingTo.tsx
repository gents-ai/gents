/* The armed reply, made visible. Opening a mailbox item with Reply arms a
   cause on the shell: the next message sent carries the item's id, and the
   item closes when that request is claimed. Without this strip the composer
   looked the same armed or not, so a person could not tell a reply from an
   ordinary message, nor put the reply down. The chip names the item, the
   popover shows what it asked, and the X disarms it. */
import { ChevronDown, Clock, CornerDownLeft, X } from "lucide-react";
import type { MailboxItemView } from "@source-inc/gents-desktop-client";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import {
  HoverCard,
  HoverCardContent,
  HoverCardTrigger,
} from "@gents/ui/components/hover-card";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { cn } from "@gents/ui/lib/utils";
import type { Shell } from "@/hooks/useShell";
import { href } from "@/lib/router";
import { useState } from "react";
import { span, when } from "./time";

const KIND_LABEL: Record<string, string> = {
  ask: "Question",
  gate: "Gate",
  finished: "Finished",
  failed: "Failed",
  flag: "Flag",
};

export function ReplyingTo({ shell }: { shell: Shell }) {
  const [open, setOpen] = useState(false);
  const cause = shell.mailboxCause;
  if (!cause) return null;
  const item = shell.selectedDeployment?.mailboxItems.find(
    (m) => m.itemId === cause.itemId,
  );
  return (
    <div
      className="flex items-center px-2 pt-2"
      data-testid="replying-to"
      data-item-id={cause.itemId}
    >
      {/* one pill: the name opens the popover, and the X, shown while the
          pointer or focus is on the pill, disarms the reply. The pill is
          capped so a long title truncates rather than widening the row */}
      <span className="group/chip inline-flex h-6 max-w-56 items-center rounded-full border border-border bg-background text-xs text-muted-foreground transition-colors focus-within:border-foreground/30 hover:border-foreground/30 has-data-popup-open:border-foreground/30">
        <HoverCard open={open} onOpenChange={setOpen}>
          {/* the card opens on hover as a preview does, and a click pins it
              open for a touch screen or a keyboard */}
          <HoverCardTrigger
            delay={300}
            render={
              <button
                type="button"
                className="inline-flex h-full min-w-0 cursor-default items-center gap-1.5 rounded-full pr-1 pl-2 outline-none group-hover/chip:text-foreground aria-expanded:text-foreground focus-visible:text-foreground data-popup-open:text-foreground"
                data-testid="replying-to-chip"
                onClick={() => setOpen((o) => !o)}
              />
            }
          >
            {/* the arrow says "reply"; the words said it again and left no
                room for the title, which is the part a person needs */}
            <CornerDownLeft className="size-3 shrink-0" />
            <span className="truncate text-foreground">
              {item?.title ?? "a mailbox item"}
            </span>
          </HoverCardTrigger>
          <HoverCardContent
            align="start"
            side="top"
            sideOffset={8}
            /* the kit's shadow is sized for a light page; over the dark transcript
               the card read flat, held apart only by its hairline */
            className="w-80 gap-3 shadow-xl shadow-black/40 md:w-[28rem]"
          >
            {item ? (
              <Details item={item} />
            ) : (
              <p>This item is no longer in the mailbox.</p>
            )}
            <div className="flex justify-end">
              <a
                href={href({ name: "mailbox" })}
                className="text-xs text-muted-foreground underline-offset-4 hover:text-foreground hover:underline"
              >
                Show in mailbox
              </a>
            </div>
          </HoverCardContent>
        </HoverCard>
        <button
          type="button"
          aria-label="Stop replying to this item"
          title="Send an ordinary message instead"
          className="mr-1 grid size-4 shrink-0 cursor-default place-items-center rounded-full text-muted-foreground opacity-0 outline-none transition-opacity duration-100 group-focus-within/chip:opacity-100 group-hover/chip:opacity-100 hover:bg-muted hover:text-foreground focus-visible:opacity-100 motion-reduce:transition-none"
          data-testid="replying-to-clear"
          onClick={() => shell.clearMailboxCause()}
        >
          <X className="size-3" />
        </button>
      </span>
    </div>
  );
}

function Details({ item }: { item: MailboxItemView }) {
  /* fixed when the popover opens: a deadline does not tick over mid-read */
  const [now] = useState(() => Date.now());
  const deadline = item.deadlineAt ? Date.parse(item.deadlineAt) : null;
  const soon = deadline !== null && deadline - now < 60 * 60_000;
  return (
    <div className="flex min-w-0 flex-col gap-2">
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        <span>{KIND_LABEL[item.kind] ?? item.kind}</span>
        <span aria-hidden="true">·</span>
        <span>{when(item.createdAt)}</span>
        {deadline !== null && (
          <span
            className={cn(
              "ml-auto inline-flex items-center gap-1",
              soon ? "text-yellow" : "text-muted-foreground",
            )}
          >
            <Clock className="size-3" />
            {`expires in ${span(Math.max(0, deadline - now))}`}
          </span>
        )}
      </div>
      <h3 className="font-heading text-sm font-medium text-heading">{item.title}</h3>
      {item.summary && <p className="text-sm text-muted-foreground">{item.summary}</p>}
      {item.action === "write_document" && item.expectedCollection && (
        <p className="text-xs text-muted-foreground">
          Expects a {item.expectedCollection} document.
        </p>
      )}
      {item.payload && (
        /* the same fold as the mailbox card: the payload is data the agent
           attached, closed until asked for, so the card stays short */
        <Collapsible className="group/details">
          <CollapsibleTrigger className="inline-flex cursor-pointer items-center gap-1 text-xs text-muted-foreground hover:text-foreground">
            Details
            <ChevronDown className="size-3 transition-transform duration-150 group-data-open/details:rotate-180 motion-reduce:transition-none" />
          </CollapsibleTrigger>
          <CollapsibleContent>
            <ScrollArea className="mt-2 max-h-40 rounded-xl border border-border/60 bg-background">
              <pre className="px-3 py-2 font-mono text-[11px] leading-relaxed whitespace-pre-wrap text-muted-foreground">
                {pretty(item.payload)}
              </pre>
            </ScrollArea>
          </CollapsibleContent>
        </Collapsible>
      )}
    </div>
  );
}

const pretty = (value: string) => {
  try {
    return JSON.stringify(JSON.parse(value), null, 2);
  } catch {
    return value;
  }
};
