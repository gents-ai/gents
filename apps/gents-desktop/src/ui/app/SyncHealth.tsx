/* Database sync at a glance, in the header: a dot and the client's own
   short label (Current, Syncing, Checking sync, Error), with the
   diagnostics the desktop's indicator shows behind a click. */
import type { SyncHealthView } from "@source-inc/gents-desktop-client";
import {
  projectSyncOperationalStatus,
  syncHealthDiagnostics,
} from "@source-inc/gents-desktop-client";
import { Popover, PopoverContent, PopoverTrigger } from "@gents/ui/components/popover";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";

export function SyncHealth({
  syncHealth,
  compact = false,
  row = false,
}: {
  syncHealth: SyncHealthView | null | undefined;
  /** the dot alone, for the rail; the label is still its name and in the details */
  compact?: boolean;
  /* as a full-width row of the nav panel */
  row?: boolean;
}) {
  const popover = useExclusivePopover();
  const status = projectSyncOperationalStatus(syncHealth);
  const d = syncHealthDiagnostics(syncHealth);
  const dot =
    status.kind === "ready"
      ? "bg-brand"
      : status.kind === "blocked"
        ? "bg-destructive"
        : status.kind === "waiting"
          ? "bg-border"
          : "bg-yellow";
  const rows: [string, string | number | null][] = [
    ["State", d.state ?? "unknown"],
    ["Connected peers", d.connectedPeerCount],
    ["Pending receive DAGs", d.pendingDagCount ?? "—"],
    ["Persisted pending DAGs", d.persistedPendingDagCount ?? "—"],
    ["Pending push retries", d.pushRetryMarkerCount ?? "—"],
    ["Exhausted fetches", d.exhaustedFetchCount ?? "—"],
    ["Quarantined DAGs", d.quarantinedDagCount ?? "—"],
  ];
  return (
    <Popover
      open={popover.open}
      onOpenChange={popover.onOpenChange}
      onOpenChangeComplete={popover.onOpenChangeComplete}
    >
      <PopoverTrigger
        aria-label={`${status.shortLabel}. Show sync diagnostics.`}
        title={status.detail}
        className={cn(
          "flex shrink-0 cursor-pointer items-center gap-2 whitespace-nowrap text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
          row
            ? /* a row of the nav panel: the dot in the icon slot, the label beside it */
              "h-8 w-full rounded-lg pr-2 text-sm"
            : "h-7 rounded-full text-xs",
          !row && (compact ? "w-7 justify-center" : "px-2"),
        )}
      >
        <span className={cn("grid shrink-0 place-items-center", row && "size-[30px]")}>
          {status.kind === "syncing" || status.kind === "working" ? (
            <Spinner className="text-foreground" />
          ) : (
            <span
              className={cn("size-2 shrink-0 rounded-full", dot)}
              aria-hidden="true"
            />
          )}
        </span>
        {/* on phones the dot is the chip; the status is still its label and
            in the details it opens */}
        {!compact && (
          <span className={cn(!row && "max-sm:hidden")}>{status.shortLabel}</span>
        )}
      </PopoverTrigger>
      <PopoverContent
        ref={popover.popupRef}
        role={popover.open ? "dialog" : "presentation"}
        aria-label={popover.open ? "Database sync details" : undefined}
        aria-hidden={popover.open ? undefined : true}
        align={compact ? "start" : "end"}
        side={compact ? "right" : "bottom"}
        className="w-96"
      >
        <p className="font-heading text-sm font-medium text-heading">Database sync</p>
        <p className="mt-0.5 text-sm text-muted-foreground">{status.detail}</p>
        <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-6 gap-y-1.5 text-sm">
          {rows.map(([k, v]) => (
            <div key={k} className="contents">
              <dt className="whitespace-nowrap text-muted-foreground">{k}</dt>
              <dd className="text-right font-mono text-xs leading-5">{v}</dd>
            </div>
          ))}
        </dl>
        {d.lastError && (
          <div className="mt-3 border-t border-border/60 pt-3">
            <p className="text-sm text-muted-foreground">Last error</p>
            <p className="mt-1 font-mono text-xs leading-5 break-all">{d.lastError}</p>
          </div>
        )}
      </PopoverContent>
    </Popover>
  );
}
