/* What a backend's provider reports it has used: a window's bar and the
   rows under a backend. */
import type {
  BackendUsageView,
  UsageWindowView,
} from "@source-inc/gents-desktop-client";
import { FactRow } from "./editors";

/* "2h13m", "3m", "<1m": the CLI's short durations */
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

/* a row's usage: its most-used window, labelled, the one that blocks first */
export function UsageBar({ view }: { view?: BackendUsageView }) {
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

/* the opened row's usage: each window, or why there is no number, and how
   this read went */
export function UsageRows({ view }: { view?: BackendUsageView }) {
  const now = Date.now();
  const read = view?.read?.startsWith("unavailable: ")
    ? `Not read: ${view.read.slice("unavailable: ".length)}`
    : view?.readError;
  return (
    <>
      {view?.windows.length ? (
        view.windows.map((w) => (
          <FactRow key={w.label} label={w.label}>
            {windowText(w, now)}
          </FactRow>
        ))
      ) : (
        <FactRow label="Reported">{view?.note ?? "unknown"}</FactRow>
      )}
      {read && <FactRow label="Last read">{read}</FactRow>}
    </>
  );
}
