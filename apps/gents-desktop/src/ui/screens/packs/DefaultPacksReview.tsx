import { useRef, useState, type ReactNode } from "react";
import { ArrowRight, ChevronRight } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { cn } from "@gents/ui/lib/utils";
import { Spinner } from "@gents/ui/components/spinner";
import type {
  DefaultPack,
  DefaultPackApproval,
} from "@source-inc/gents-desktop-client";
import { preferences, useStarterPacksDismissed } from "@/preferences";
import { href } from "@/lib/router";
import { shortName } from "./model";
import { ErrorDetail, Section } from "./PackCard";
import {
  isPending,
  useDefaultPacks,
  type DefaultPacksReview as Review,
} from "./useDefaultPacks";

const TITLE = "Starter packs";
const INTRO = "The packs the Engineer starts with. They come with Gents.";

/* what the button on a pending pack does */
const actionOf = (pack: DefaultPack) =>
  pack.state === "missing" ? "Install" : "Turn on";

const LABEL = "text-xs text-muted-foreground";

/** The starter packs not yet installed or turned on, one row each: what it
    is for, opening to what the Engineer lacks without it, with Install on a
    pack this home is missing and Turn on on one that is installed and off.
    Every word about a pack is the bridge's. With `onDismiss` it is the card:
    Not now sits beside Install all, and the list scrolls between the heading
    and those buttons. */
export function DefaultPacksReview({
  review,
  onDismiss,
}: {
  review: Review;
  onDismiss?: () => void;
}) {
  const { packs, busy, failed, error, approve, retry } = review;
  const open = packs.filter((pack) => pack.state !== "installed");
  const missing: DefaultPackApproval[] = open
    .filter((pack) => pack.state === "missing")
    .map((pack) => ({ pack: pack.pack }));
  const List = onDismiss ? CardList : PageList;

  return (
    <div
      /* on a page the packs and their button sit in one card, apart from
         what is installed */
      className={cn(
        "grid grid-cols-[minmax(0,1fr)] gap-3",
        !onDismiss && "rounded-2xl border border-border/60 bg-raised px-4 pt-2 pb-3",
      )}
      data-testid="starter-packs"
    >
      <List>
        {open.length > 0 && (
          <ul aria-label={TITLE} className="divide-y divide-border/60">
            {open.map((pack) => {
              const action = actionOf(pack);
              const failure = failed.find((f) => f.pack === pack.pack);
              return (
                <li key={pack.pack} className="py-1">
                  <Collapsible className="group/about">
                    <div className="flex items-center gap-3">
                      <CollapsibleTrigger className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 py-1.5 text-left">
                        <ChevronRight className="size-3.5 shrink-0 text-muted-foreground transition-transform duration-150 group-data-open/about:rotate-90 motion-reduce:transition-none" />
                        <span className="shrink-0 text-sm font-medium">
                          {shortName(pack.pack)}
                        </span>{" "}
                        <span className="min-w-0 truncate text-sm text-muted-foreground first-letter:uppercase">
                          {pack.purpose}
                        </span>
                      </CollapsibleTrigger>
                      {isPending(pack) && (
                        <Button
                          size="sm"
                          variant="ghost"
                          className="text-xs text-muted-foreground hover:text-foreground"
                          disabled={busy}
                          aria-label={`${action} ${pack.pack}`}
                          onClick={() => void approve([{ pack: pack.pack }])}
                        >
                          {action}
                        </Button>
                      )}
                    </div>
                    {pack.state === "unavailable" && (
                      <p className="pb-1.5 pl-5.5 text-sm text-warning">
                        {pack.reason ??
                          "This build of Gents doesn't include this pack."}
                      </p>
                    )}
                    <CollapsibleContent className="h-(--collapsible-panel-height) overflow-hidden pl-5.5 transition-[height,opacity] duration-200 ease-out data-ending-style:h-0 data-ending-style:opacity-0 data-starting-style:h-0 data-starting-style:opacity-0 motion-reduce:transition-none">
                      <div className="mb-2 grid gap-3 rounded-xl bg-muted/40 p-3">
                        <p className={LABEL}>
                          {pack.without} <span className="font-mono">{pack.pack}</span>
                        </p>
                        {!onDismiss && (
                          <a
                            href={href({ name: "packs", item: pack.pack })}
                            className="inline-flex w-max items-center gap-1 text-xs text-muted-foreground underline decoration-border underline-offset-4 hover:text-foreground"
                          >
                            View {shortName(pack.pack)}’s page
                            <ArrowRight className="size-3" />
                          </a>
                        )}
                      </div>
                    </CollapsibleContent>
                  </Collapsible>
                  {failure && (
                    <div role="alert" className="grid gap-1">
                      <p className="text-sm text-warning">
                        {failure.installed
                          ? `${shortName(pack.pack)} is installed, but setting it up for the Engineer didn't finish. Select ${action} again to finish.`
                          : `Couldn't install ${shortName(pack.pack)}. Nothing changed for it; you can try again.`}
                      </p>
                      <ErrorDetail error={failure.error} />
                    </div>
                  )}
                </li>
              );
            })}
          </ul>
        )}

        {error && (
          <div role="alert" className="grid gap-1">
            <p className="text-sm text-warning">
              {open.length > 0
                ? "Couldn't set up the packs."
                : "Couldn't read the starter packs."}
            </p>
            <ErrorDetail error={error} />
            {open.length === 0 && (
              <Button
                size="sm"
                variant="outline"
                className="w-max"
                onClick={() => void retry()}
              >
                Retry
              </Button>
            )}
          </div>
        )}
      </List>

      {busy && (
        <p
          role="status"
          className="flex items-center gap-2 text-sm text-muted-foreground"
        >
          <Spinner /> The local node restarts for a moment while the packs are set up.
        </p>
      )}

      {(missing.length > 0 || onDismiss) && (
        <div
          className={cn(
            "flex flex-col-reverse gap-2 sm:flex-row sm:justify-end",
            !onDismiss && "border-t border-border/60 pt-3",
          )}
        >
          {onDismiss && (
            <Button variant="outline" disabled={busy} onClick={onDismiss}>
              Not now
            </Button>
          )}
          {missing.length > 0 && (
            <Button
              variant="brand"
              disabled={busy}
              onClick={() => void approve(missing)}
            >
              {busy ? "Installing…" : "Install all"}
            </Button>
          )}
        </div>
      )}
    </div>
  );
}

const CardList = ({ children }: { children: ReactNode }) => (
  <ScrollArea className="-mr-3 max-h-[55vh]">
    <div className="grid grid-cols-[minmax(0,1fr)] gap-4 pr-3">{children}</div>
  </ScrollArea>
);

const PageList = ({ children }: { children: ReactNode }) => (
  <div className="grid grid-cols-[minmax(0,1fr)] gap-4">{children}</div>
);

/** The starter packs offered once over the shell on the desktop's own node
    while one is missing from it, until they are installed or put off with
    Not now. Putting them off is remembered on this device for that node; the
    Packs page keeps them, hidden to one line until Show. A pack that is
    installed and off, or that this build doesn't carry, doesn't open it by
    itself. */
export function StarterPacksCard() {
  const review = useDefaultPacks();
  const dismissed = useStarterPacksDismissed(review.nodeDid);
  /* the card opens unasked, so it takes focus itself and marks no pack */
  const card = useRef<HTMLDivElement>(null);
  const { nodeDid, busy } = review;
  const missing = review.packs.some((pack) => pack.state === "missing");
  const open = nodeDid !== null && !dismissed && missing;
  const dismiss = () => {
    if (nodeDid !== null && !busy) preferences.dismissStarterPacks(nodeDid);
  };
  return (
    <Dialog open={open} onOpenChange={(next) => !next && dismiss()}>
      <DialogContent
        ref={card}
        initialFocus={card}
        /* it opens unasked over an app not yet seen, so that app shows through */
        overlayProps={{
          className: "bg-black/10 supports-backdrop-filter:backdrop-blur-[1.5px]",
        }}
        className="outline-none sm:max-w-xl"
        data-testid="starter-packs-card"
      >
        <DialogHeader>
          <DialogTitle>{TITLE}</DialogTitle>
          <DialogDescription>{INTRO}</DialogDescription>
        </DialogHeader>
        <DefaultPacksReview review={review} onDismiss={dismiss} />
      </DialogContent>
    </Dialog>
  );
}

/** The same review as a section of the Packs page, shown while any starter
    pack is not installed and on, or the list could not be read. Hiding it puts the
    packs off as Not now on the card does, leaving one line; Show opens them
    here again without bringing the card back. Like Not now, Hide waits while
    an install runs. A list that could not be read has nothing to hide, so its
    error shows even when the packs are put off. */
export function StarterPacksSection() {
  const review = useDefaultPacks();
  const { nodeDid, busy } = review;
  const dismissed = useStarterPacksDismissed(nodeDid);
  const [shown, setShown] = useState(false);
  const waiting = review.packs.filter((pack) => pack.state !== "installed");
  if (waiting.length === 0 && !review.error) return null;
  const hidden = dismissed && !shown && waiting.length > 0;
  const toggle =
    nodeDid === null || waiting.length === 0 ? null : (
      <Button
        size="sm"
        variant="ghost"
        className="text-xs text-muted-foreground hover:text-foreground"
        aria-expanded={!hidden}
        disabled={busy}
        onClick={() => {
          if (hidden) setShown(true);
          else {
            setShown(false);
            preferences.dismissStarterPacks(nodeDid);
          }
        }}
      >
        {hidden ? "Show" : "Hide"}
      </Button>
    );
  return (
    <Section title={TITLE} action={toggle}>
      {hidden ? (
        <p className="text-sm text-muted-foreground">
          {waiting.length === 1
            ? "1 starter pack isn’t set up."
            : `${waiting.length} starter packs aren’t set up.`}
        </p>
      ) : (
        <>
          <p className="text-sm text-muted-foreground">{INTRO}</p>
          <DefaultPacksReview review={review} />
        </>
      )}
    </Section>
  );
}
