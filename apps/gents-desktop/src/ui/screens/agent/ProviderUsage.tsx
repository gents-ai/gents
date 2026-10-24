import { useRef, useState } from "react";
import { Button } from "@gents/ui/components/button";
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

function windowText(w: UsageWindowView, now: number) {
  const parts = [`${Math.round(w.usedPct)}% used`];
  if (w.resetsAt) {
    const at = new Date(w.resetsAt);
    const time = at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    parts.push(`resets in ${shortDuration(at.getTime() - now)} (${time})`);
  }
  parts.push(
    `${USAGE_SOURCE[w.source] ?? w.source}, ${shortDuration(now - Date.parse(w.observedAt))} ago`,
  );
  if (w.lastKnown) parts.push("last known");
  return parts.join(" · ");
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
          <section key={label} aria-label={label}>
            <p className="text-xs font-medium text-muted-foreground">{label}</p>
            {windows.map((w) => (
              <FactRow key={w.label} label={w.label}>
                {windowText(w, now)}
              </FactRow>
            ))}
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
      className="mb-6 space-y-4 rounded-lg border border-border p-4"
    >
      <div className="flex items-center justify-between gap-3">
        <h2 className="font-medium">Your limits</h2>
        <Button
          size="sm"
          variant="outline"
          disabled={busy}
          onClick={() => void refresh()}
        >
          {busy ? "Refreshing…" : "Refresh all limits"}
        </Button>
      </div>
      {!views.length && (
        <p className="text-sm text-muted-foreground">
          No account limits have been reported.
        </p>
      )}
      {views.map((view, index) => (
        <div key={`${view.provider}:${view.label}:${index}`} className="space-y-2">
          <h3 className="text-sm font-medium">
            {view.label}{" "}
            <span className="font-normal text-muted-foreground">
              ·{" "}
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
      {error && (
        <p role="alert" className="text-sm">
          {error}
        </p>
      )}
    </section>
  );
}
