import { useRef, type ReactNode } from "react";
import { ChevronRight } from "lucide-react";
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
import { shortName } from "./model";
import { ErrorDetail, Section } from "./PackCard";
import {
  isPending,
  useDefaultPacks,
  type DefaultPacksReview as Review,
} from "./useDefaultPacks";

const TITLE = "Starter packs";
const INTRO = "The packs the Engineer starts with. Installing them needs the network.";

/* an approval names the archive that was shown; a pack with none can't be approved */
const approvalOf = (pack: DefaultPack): DefaultPackApproval | null =>
  isPending(pack) && pack.digest ? { pack: pack.pack, digest: pack.digest } : null;

const LABEL = "text-xs text-muted-foreground";

/** The starter packs not yet installed: what each is for, the permissions it
    asks for, what approving turns on and what declining costs, with Approve
    on each and on all of them. Every word about a pack is the bridge's.
    With `onDismiss` it is the card: Not now sits beside Approve all, and the
    list scrolls between the heading and those buttons. */
export function DefaultPacksReview({
  review,
  onDismiss,
}: {
  review: Review;
  onDismiss?: () => void;
}) {
  const { packs, busy, failed, error, approve, retry } = review;
  const open = packs.filter((pack) => pack.state !== "installed");
  const all = open.flatMap((pack) => approvalOf(pack) ?? []);
  const List = onDismiss ? CardList : PageList;
  /* on a page there is no card to draw once nothing is left to offer */
  if (!onDismiss && open.length === 0 && !error) return null;

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
              const approval = approvalOf(pack);
              const failure = failed.find((f) => f.pack === pack.pack);
              return (
                <li key={pack.pack} className="py-1">
                  <Collapsible className="group/asks">
                    <div className="flex items-center gap-3">
                      <CollapsibleTrigger
                        aria-label={`What ${pack.pack} asks for`}
                        className="flex min-w-0 flex-1 cursor-pointer items-center gap-2 py-1.5 text-left"
                      >
                        <ChevronRight className="size-3.5 shrink-0 text-muted-foreground transition-transform duration-150 group-data-open/asks:rotate-90 motion-reduce:transition-none" />
                        <span className="shrink-0 text-sm font-medium">
                          {shortName(pack.pack)}
                        </span>
                        <span className="min-w-0 truncate text-sm text-muted-foreground first-letter:uppercase">
                          {pack.purpose}
                        </span>
                      </CollapsibleTrigger>
                      {pack.state === "unavailable" ? (
                        <Button
                          size="sm"
                          variant="ghost"
                          className="text-xs text-muted-foreground hover:text-foreground"
                          disabled={busy}
                          aria-label={`Retry ${pack.pack}`}
                          onClick={() => void retry()}
                        >
                          Retry
                        </Button>
                      ) : (
                        <Button
                          size="sm"
                          variant="ghost"
                          className="text-xs text-muted-foreground hover:text-foreground"
                          disabled={busy || approval === null}
                          aria-label={`${pack.state === "ready" ? "Install" : "Approve"} ${pack.pack}`}
                          onClick={() => approval && void approve([approval])}
                        >
                          {pack.state === "ready" ? "Install" : "Approve"}
                        </Button>
                      )}
                    </div>
                    {pack.state === "unavailable" && (
                      <p className="pb-1.5 pl-5.5 text-sm text-warning">
                        {pack.reason ?? "This pack can't be installed right now."}
                      </p>
                    )}
                    <CollapsibleContent className="h-(--collapsible-panel-height) overflow-hidden pl-5.5 transition-[height,opacity] duration-200 ease-out data-ending-style:h-0 data-ending-style:opacity-0 data-starting-style:h-0 data-starting-style:opacity-0 motion-reduce:transition-none">
                      <div className="mb-2 grid gap-3 rounded-xl bg-muted/40 p-3">
                        {pack.requested.length > 0 && (
                          <div className="grid gap-1.5">
                            <span className={LABEL}>Permissions it asks for</span>
                            <Asks asks={pack.requested} />
                          </div>
                        )}
                        {pack.turns_on.length > 0 && (
                          <div className="grid gap-1.5">
                            <span className={LABEL}>Approving also turns on</span>
                            <ul className="grid list-disc gap-1 pl-4 text-xs marker:text-muted-foreground">
                              {pack.turns_on.map((sentence) => (
                                <li key={sentence}>{sentence}</li>
                              ))}
                            </ul>
                          </div>
                        )}
                        <p className={LABEL}>
                          {pack.without} <span className="font-mono">{pack.pack}</span>
                        </p>
                      </div>
                    </CollapsibleContent>
                  </Collapsible>
                  {failure && (
                    <div role="alert" className="grid gap-1">
                      <p className="text-sm text-warning">
                        {failure.installed
                          ? `${shortName(pack.pack)} is installed with its permissions granted, but setting it up for the Engineer didn't finish. Approve it again to finish.`
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
                ? "Couldn't install the packs."
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
          <Spinner /> The local node restarts for a moment while the packs install.
        </p>
      )}

      {(all.length > 0 || onDismiss) && (
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
          {all.length > 0 && (
            <Button variant="brand" disabled={busy} onClick={() => void approve(all)}>
              {busy ? "Installing…" : "Approve all"}
            </Button>
          )}
        </div>
      )}
    </div>
  );
}

/** Each tool beside what it may reach, its resource limits on a quieter
    line. The bridge writes one sentence per tool, clauses parted by "; ",
    the limits clause opening "up to"; a sentence shaped otherwise is shown
    whole. */
function Asks({ asks }: { asks: DefaultPack["requested"] }) {
  return (
    <dl className="grid grid-cols-[minmax(0,auto)_minmax(0,1fr)] gap-x-4 gap-y-2 text-xs">
      {asks.map((ask) => {
        const clauses = ask.asks.split("; ");
        const reach = clauses.filter((clause) => !clause.startsWith("up to "));
        const caps = clauses
          .filter((clause) => clause.startsWith("up to "))
          .map((clause) => clause.slice("up to ".length).split(", ").join(" · "));
        return (
          <div key={ask.plugin} className="col-span-2 grid grid-cols-subgrid">
            <dt className="truncate font-mono">{ask.plugin}</dt>
            <dd className="grid gap-0.5">
              {reach.map((clause) => (
                <span key={clause} className="first-letter:uppercase">
                  {clause}
                </span>
              ))}
              {caps.map((clause) => (
                <span key={clause} className={LABEL}>
                  Up to {clause}
                </span>
              ))}
            </dd>
          </div>
        );
      })}
    </dl>
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

/** The starter packs offered once over the shell on the desktop's own node,
    until they are installed or put off with Not now. Putting them off is
    remembered on this device for that node; the Packs page keeps offering
    them. Packs that could not be resolved don't open it by themselves. */
export function StarterPacksCard() {
  const review = useDefaultPacks();
  const dismissed = useStarterPacksDismissed(review.nodeDid);
  /* the card opens unasked, so it takes focus itself and marks no pack */
  const card = useRef<HTMLDivElement>(null);
  const { nodeDid, busy } = review;
  const open = nodeDid !== null && !dismissed && review.packs.some(isPending);
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
    pack is not installed or the list could not be read. */
export function StarterPacksSection() {
  const review = useDefaultPacks();
  const waiting = review.packs.some((pack) => pack.state !== "installed");
  if (!waiting && !review.error) return null;
  return (
    <Section title={TITLE}>
      <p className="text-sm text-muted-foreground">{INTRO}</p>
      <DefaultPacksReview review={review} />
    </Section>
  );
}
