import { useRef, useState } from "react";
import { RotateCw } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import { Hint } from "@/screens/Hint";
import { setupErrorMessage } from "../../../lib/setupErrors";
import type {
  AccountUsageView,
  UsageWindowView,
} from "@source-inc/gents-desktop-client";
import { FactRow, LongFactRow } from "./editors";
import { KINDS, SUBSCRIPTION } from "./inferenceKinds";

function shortDuration(ms: number) {
  const total = Math.max(0, Math.floor(ms / 60_000));
  const [days, hours, minutes] = [
    Math.floor(total / 1440),
    Math.floor((total % 1440) / 60),
    total % 60,
  ];
  if (days) return hours ? `${days}d${hours}h` : `${days}d`;
  if (hours) return minutes ? `${hours}h${minutes}m` : `${hours}h`;
  return minutes ? `${minutes}m` : "<1m";
}

const USAGE_SOURCE: Record<string, string> = {
  header: "from response headers",
  endpoint: "from the usage endpoint",
  error: "from a rejected request",
};

/* what the row leaves out, for its tooltip: when it resets and where the
   figure came from */
function windowDetail(w: UsageWindowView, now: number) {
  const parts: string[] = [];
  if (w.resetsAt)
    parts.push(
      `Resets ${new Date(w.resetsAt).toLocaleString([], {
        weekday: "short",
        hour: "numeric",
        minute: "2-digit",
      })}`,
    );
  parts.push(
    `${USAGE_SOURCE[w.source] ?? w.source}, ${shortDuration(now - Date.parse(w.observedAt))} ago`,
  );
  if (w.lastKnown) parts.push("last known");
  return parts.join(" · ");
}

/* one window: its name, how full it is and when it empties */
function WindowRow({ w, now }: { w: UsageWindowView; now: number }) {
  const pct = Math.round(w.usedPct);
  return (
    <li
      title={windowDetail(w, now)}
      className="grid grid-cols-[minmax(2.5rem,auto)_minmax(3rem,1fr)_3ch_minmax(0,auto)] items-center gap-3 text-sm"
    >
      <span className="truncate text-foreground">{w.label}</span>
      <span
        role="meter"
        aria-label={`${w.label} used`}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={pct}
        className="h-1.5 overflow-hidden rounded-full bg-muted"
      >
        <span
          className={cn(
            "block h-full rounded-full",
            pct >= 90 ? "bg-destructive" : "bg-foreground/60",
          )}
          style={{ width: `${Math.min(pct, 100)}%` }}
        />
      </span>
      <span className="text-right text-foreground tabular-nums">{pct}%</span>
      <span className="truncate text-xs text-muted-foreground tabular-nums">
        {w.resetsAt
          ? `resets in ${shortDuration(Date.parse(w.resetsAt) - now)}`
          : w.lastKnown
            ? "last known"
            : ""}
      </span>
    </li>
  );
}

export function UsageBar({ view }: { view?: AccountUsageView }) {
  const top = view?.windows.reduce<UsageWindowView | undefined>(
    (most, w) => (!most || w.usedPct > most.usedPct ? w : most),
    undefined,
  );
  if (!top) return null;
  const pct = Math.round(top.usedPct);
  return (
    <span className="flex items-center gap-1.5 px-1 text-xs text-muted-foreground tabular-nums">
      {top.label}{" "}
      <span aria-hidden className="h-1.5 w-12 overflow-hidden rounded-full bg-muted">
        <span
          className="block h-full bg-foreground/60"
          style={{ width: `${Math.min(pct, 100)}%` }}
        />
      </span>
      {pct}%
    </span>
  );
}

export function usageWindowGroup(window: UsageWindowView) {
  if (window.label.includes("tokens")) return "Token limits";
  if (window.label === "requests") return "Request limits";
  if (["daily", "weekly", "monthly", "credits"].includes(window.label))
    return "Spending limits";
  return "Quota windows";
}

export function UsageRows({ view }: { view?: AccountUsageView }) {
  const now = Date.now();
  const read = view?.read?.startsWith("unavailable: ")
    ? `Not read: ${view.read.slice("unavailable: ".length)}`
    : view?.readError;
  const groups = new Map<string, UsageWindowView[]>();
  for (const window of view?.windows ?? []) {
    const label = usageWindowGroup(window);
    groups.set(label, [...(groups.get(label) ?? []), window]);
  }
  return (
    <>
      {view?.windows.length ? (
        Array.from(groups, ([label, windows]) => (
          <section key={label} aria-label={label} className="grid gap-1.5">
            {groups.size > 1 && (
              <p className="text-xs text-muted-foreground">{label}</p>
            )}
            <ul className="grid gap-1.5">
              {windows.map((w) => (
                <WindowRow key={w.label} w={w} now={now} />
              ))}
            </ul>
          </section>
        ))
      ) : (
        <FactRow label="Reported">{view?.note ?? "unknown"}</FactRow>
      )}
      {read && <LongFactRow label="Last read">{read}</LongFactRow>}
    </>
  );
}

export function AllProviderLimits({
  views,
  onRefresh,
}: {
  views: readonly AccountUsageView[];
  onRefresh: () => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);
  const [error, setError] = useState("");
  const refresh = async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setError("");
    try {
      await onRefresh();
    } catch (failed) {
      setError(setupErrorMessage(failed));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };
  return (
    <section
      aria-label="Your limits"
      className="mb-6 grid gap-3 rounded-xl border border-border px-4 pt-2.5 pb-3.5"
    >
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-sm font-medium">Your limits</h2>
        <Hint label={busy ? "Refreshing…" : "Refresh all limits"}>
          <Button
            size="icon-sm"
            variant="ghost"
            aria-label="Refresh all limits"
            disabled={busy}
            onClick={() => void refresh()}
          >
            <RotateCw className={cn(busy && "animate-spin")} />
          </Button>
        </Hint>
      </div>
      {!views.length && (
        <p className="text-sm text-muted-foreground">
          No account limits have been reported.
        </p>
      )}
      <div className="grid divide-y divide-border/60">
        {views.map((view, index) => (
          <div
            key={`${view.provider}:${view.label}:${index}`}
            className="grid gap-2 py-3 first:pt-0 last:pb-0"
          >
            <h3 className="text-xs font-medium text-muted-foreground">
              <span className="text-foreground">{view.label}</span>{" "}
              <span className="font-normal">
                {Object.values(SUBSCRIPTION).find(
                  (entry) => entry.provider === view.provider,
                )?.title ??
                  KINDS.find((entry) => entry.value === view.provider)?.label ??
                  view.provider}
              </span>
            </h3>
            {view.read === "disabled" ? (
              <p className="text-sm text-muted-foreground">Disabled</p>
            ) : (
              <UsageRows view={view} />
            )}
          </div>
        ))}
      </div>
      {error && (
        <p role="alert" className="text-sm">
          {error}
        </p>
      )}
    </section>
  );
}
