import type { SyncHealthView } from "@source-inc/gents-desktop-client";
import {
  projectSyncOperationalStatus,
  syncHealthDiagnostics,
} from "@source-inc/gents-desktop-client";
import { useState } from "react";
import { Popover, PopoverContent, PopoverTrigger } from "@gents/ui/components/popover";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";

type SyncStatus = ReturnType<typeof projectSyncOperationalStatus>;

/** the status dot's colour: green ready, red blocked, grey waiting, else yellow */
export function syncDotClass(status: SyncStatus): string {
  return status.kind === "ready"
    ? "bg-brand"
    : status.kind === "blocked"
      ? "bg-destructive"
      : status.kind === "waiting"
        ? "bg-border"
        : "bg-yellow";
}

function StatusMark({ status, row }: { status: SyncStatus; row: boolean }) {
  return (
    <span className={cn("grid shrink-0 place-items-center", row && "size-[30px]")}>
      {status.kind === "syncing" || status.kind === "working" ? (
        <Spinner className="text-foreground" />
      ) : (
        <span
          className={cn("size-2 shrink-0 rounded-full", syncDotClass(status))}
          aria-hidden="true"
        />
      )}
    </span>
  );
}

function ConnectionDetails({
  syncHealth,
  status,
}: {
  syncHealth: SyncHealthView | null;
  status: SyncStatus;
}) {
  const d = syncHealthDiagnostics(syncHealth);
  const rows: [string, string | number | null][] = [
    ["State", d.state ?? "unknown"],
    ["Cause", d.cause ?? "—"],
    ["Next action", d.nextAction ?? "—"],
    ["Runtimes connected", `${d.connectedRuntimeCount} of ${d.runtimeCount}`],
  ];
  return (
    <>
      <p className="font-heading text-sm font-medium text-heading">Connection</p>
      <p className="mt-0.5 text-sm text-muted-foreground">{status.detail}</p>
      <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-6 gap-y-1.5 text-sm">
        {rows.map(([k, v]) => (
          <div key={k} className="contents">
            <dt className="whitespace-nowrap text-muted-foreground">{k}</dt>
            <dd className="text-right font-mono text-xs leading-5">{v}</dd>
          </div>
        ))}
      </dl>
    </>
  );
}

const ROW = "h-8 w-full rounded-lg pr-2 text-xs text-muted-foreground/80";

/* inside the narrow window's menu sheet the details unfold in place: a
   popover would be a second dialog over the sheet, and opening it closes
   the sheet that holds its trigger */
function InlineSyncHealth({ syncHealth }: { syncHealth: SyncHealthView | null }) {
  const [open, setOpen] = useState(false);
  const status = projectSyncOperationalStatus(syncHealth);
  return (
    <>
      <button
        type="button"
        aria-label={`${status.shortLabel}. Show connection details.`}
        aria-expanded={open}
        title={status.detail}
        onClick={() => setOpen(!open)}
        className={cn(
          "flex shrink-0 cursor-pointer items-center gap-2 whitespace-nowrap transition-colors hover:bg-accent hover:text-foreground",
          ROW,
        )}
      >
        <StatusMark status={status} row />
        <span className="min-w-0 truncate">{status.shortLabel}</span>
      </button>
      {open && (
        <div
          role="region"
          aria-label="Connection details"
          className="mt-1 rounded-lg bg-accent/40 p-3"
        >
          <ConnectionDetails syncHealth={syncHealth} status={status} />
        </div>
      )}
    </>
  );
}

export function SyncHealth({
  syncHealth,
  compact = false,
  row = false,
  inline = false,
}: {
  syncHealth: SyncHealthView | null;
  compact?: boolean;
  row?: boolean;
  /** a row whose details unfold below it rather than in a popover */
  inline?: boolean;
}) {
  if (inline) return <InlineSyncHealth syncHealth={syncHealth} />;
  return <PopoverSyncHealth syncHealth={syncHealth} compact={compact} row={row} />;
}

function PopoverSyncHealth({
  syncHealth,
  compact,
  row,
}: {
  syncHealth: SyncHealthView | null;
  compact: boolean;
  row: boolean;
}) {
  const popover = useExclusivePopover();
  const status = projectSyncOperationalStatus(syncHealth);
  return (
    <Popover
      open={popover.open}
      onOpenChange={popover.onOpenChange}
      onOpenChangeComplete={popover.onOpenChangeComplete}
    >
      <PopoverTrigger
        aria-label={`${status.shortLabel}. Show connection details.`}
        title={status.detail}
        className={cn(
          "flex shrink-0 cursor-pointer items-center gap-2 whitespace-nowrap text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
          /* in the nav a status, not a destination: quieter than the items
             around it */
          row ? ROW : "h-7 rounded-full text-xs",
          !row && (compact ? "w-7 justify-center" : "px-2"),
        )}
      >
        <StatusMark status={status} row={row} />
        {!compact && (
          <span className={cn(row ? "min-w-0 truncate" : "max-sm:hidden")}>
            {status.shortLabel}
          </span>
        )}
      </PopoverTrigger>
      <PopoverContent
        ref={popover.popupRef}
        role={popover.open ? "dialog" : "presentation"}
        aria-label={popover.open ? "Connection details" : undefined}
        aria-hidden={popover.open ? undefined : true}
        align={compact ? "start" : "end"}
        side={compact ? "right" : "bottom"}
        className="w-96"
      >
        <ConnectionDetails syncHealth={syncHealth} status={status} />
      </PopoverContent>
    </Popover>
  );
}
