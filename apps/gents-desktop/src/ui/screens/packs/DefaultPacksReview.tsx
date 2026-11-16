import type { ReactNode } from "react";
import { Button } from "@gents/ui/components/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { Spinner } from "@gents/ui/components/spinner";
import type {
  DefaultPack,
  DefaultPackApproval,
} from "@source-inc/gents-desktop-client";
import { preferences, useStarterPacksDismissed } from "@/preferences";
import { namespaceOf, shortName } from "./model";
import { ErrorDetail, Section } from "./PackCard";
import {
  isPending,
  useDefaultPacks,
  type DefaultPacksReview as Review,
} from "./useDefaultPacks";

const TITLE = "Starter packs";
const INTRO = "The packs the Engineer starts with. They come with Gents.";

const LABEL = "text-xs text-muted-foreground";

/* what the button on a pending pack does */
const actionOf = (pack: DefaultPack) =>
  pack.state === "missing" ? "Install" : "Turn on";

/** The starter packs not yet installed or turned on: what each is for and
    what the Engineer lacks without it, with Install on a pack this home is
    missing and Turn on on one that is installed and off. Every word about a
    pack is the bridge's. With `onDismiss` it is the card: Not now sits
    beside Install all, and the list scrolls between the heading and those
    buttons. */
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
    <div className="grid gap-4" data-testid="starter-packs">
      <List>
        {open.length > 0 && (
          <ul aria-label={TITLE} className="divide-y divide-border/60">
            {open.map((pack) => {
              const failure = failed.find((f) => f.pack === pack.pack);
              const action = actionOf(pack);
              return (
                <li key={pack.pack} className="grid gap-2 py-3 first:pt-0">
                  <div className="flex items-start gap-3">
                    <div className="grid min-w-0 flex-1 gap-0.5">
                      <span className="flex min-w-0 items-baseline gap-2">
                        <span className="truncate text-sm font-medium">
                          {shortName(pack.pack)}
                        </span>
                        <span className="shrink-0 text-xs text-muted-foreground">
                          {namespaceOf(pack.pack)}
                        </span>
                      </span>
                      <p className="text-sm text-muted-foreground first-letter:uppercase">
                        {pack.purpose}
                      </p>
                    </div>
                    {isPending(pack) && (
                      <Button
                        size="sm"
                        variant="outline"
                        disabled={busy}
                        aria-label={`${action} ${pack.pack}`}
                        onClick={() => void approve([{ pack: pack.pack }])}
                      >
                        {action}
                      </Button>
                    )}
                  </div>
                  {pack.state === "unavailable" && (
                    <p className="text-sm text-warning">
                      {pack.reason ?? "This build of Gents doesn't include this pack."}
                    </p>
                  )}
                  <p className={LABEL}>{pack.without}</p>
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
        <div className="flex flex-col-reverse gap-2 sm:flex-row sm:justify-end">
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
  <div className="grid gap-4">{children}</div>
);

/** The starter packs offered once over the shell on the desktop's own node
    while one is missing from it, until they are installed or put off with
    Not now. Putting them off is remembered on this device for that node; the
    Packs page keeps offering them. A pack that is installed and off, or that
    this build doesn't carry, doesn't open it by itself. */
export function StarterPacksCard() {
  const review = useDefaultPacks();
  const dismissed = useStarterPacksDismissed(review.nodeDid);
  const { nodeDid, busy } = review;
  const missing = review.packs.some((pack) => pack.state === "missing");
  const open = nodeDid !== null && !dismissed && missing;
  const dismiss = () => {
    if (nodeDid !== null && !busy) preferences.dismissStarterPacks(nodeDid);
  };
  return (
    <Dialog open={open} onOpenChange={(next) => !next && dismiss()}>
      <DialogContent className="sm:max-w-xl" data-testid="starter-packs-card">
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
    pack is not installed and on, or the list could not be read. */
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
