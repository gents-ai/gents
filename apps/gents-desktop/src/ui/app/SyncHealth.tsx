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

export function SyncHealth({
  syncHealth,
}: {
  syncHealth: SyncHealthView | null | undefined;
}) {
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
    <Popover>
      <PopoverTrigger
        aria-label={`${status.shortLabel}. Show sync diagnostics.`}
        title={status.detail}
        className="flex h-7 items-center gap-2 rounded-full px-2 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
      >
        {status.kind === "syncing" || status.kind === "working" ? (
          <Spinner className="text-foreground" />
        ) : (
          <span className={cn("size-2 rounded-full", dot)} aria-hidden="true" />
        )}
        {status.shortLabel}
      </PopoverTrigger>
      <PopoverContent align="end" className="w-96">
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
