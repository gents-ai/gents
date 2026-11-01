import { useRef, useState } from "react";
import { RotateCw } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Skeleton } from "@gents/ui/components/skeleton";
import { cn } from "@gents/ui/lib/utils";
import { Hint } from "@/screens/Hint";
import { setupErrorMessage } from "../../../lib/setupErrors";
import type {
  AccountUsageView,
  UsageWindowView,
} from "@source-inc/gents-desktop-client";
import { ProviderLogo } from "../ProviderLogo";
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

/* how close a window is to its limit: quiet until it is worth a look */
export function usageLevel(pct: number) {
  return pct >= 90 ? "danger" : pct >= 75 ? "warning" : "ok";
}
const FILL = {
  ok: "bg-foreground/60",
  warning: "bg-warning",
  danger: "bg-destructive",
} as const;
const STROKE = {
  ok: "stroke-foreground/60",
  warning: "stroke-warning",
  danger: "stroke-destructive",
} as const;
const INK = {
  ok: "text-foreground",
  warning: "text-warning",
  danger: "text-destructive",
} as const;

export function resetsIn(w: UsageWindowView, now: number) {
  return w.resetsAt ? `resets in ${shortDuration(Date.parse(w.resetsAt) - now)}` : null;
}

/** the window of an account closest to its limit */
export function closestWindow(view?: AccountUsageView) {
  return view?.windows.reduce<UsageWindowView | undefined>(
    (most, w) => (!most || w.usedPct > most.usedPct ? w : most),
    undefined,
  );
}

/** A small ring filled to a window's use, for places with room for a glance only. */
export function UsageRing({ pct, className }: { pct: number; className?: string }) {
  const r = 5;
  const around = 2 * Math.PI * r;
  return (
    <svg
      viewBox="0 0 14 14"
      aria-hidden
      className={cn("size-3.5 shrink-0 -rotate-90", className)}
    >
      <circle
        cx="7"
        cy="7"
        r={r}
        fill="none"
        strokeWidth="2.5"
        className="stroke-foreground/15"
      />
      <circle
        cx="7"
        cy="7"
        r={r}
        fill="none"
        strokeWidth="2.5"
        strokeLinecap="round"
        strokeDasharray={`${(around * Math.min(pct, 100)) / 100} ${around}`}
        className={STROKE[usageLevel(pct)]}
      />
    </svg>
  );
}

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

function Meter({ w, pct }: { w: UsageWindowView; pct: number }) {
  return (
    <span
      role="meter"
      aria-label={`${w.label} used`}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={pct}
      className="block h-1.5 overflow-hidden rounded-full bg-foreground/10"
    >
      <span
        className={cn("block h-full rounded-full", FILL[usageLevel(pct)])}
        style={{ width: `${Math.min(pct, 100)}%` }}
      />
    </span>
  );
}

/* one window as a row: its name, how full it is and when it empties, in
   columns the account's other rows share, so their bars end together */
function WindowRow({ w, now }: { w: UsageWindowView; now: number }) {
  const pct = Math.round(w.usedPct);
  return (
    <li
      title={windowDetail(w, now)}
      className="col-span-4 grid grid-cols-subgrid items-center text-xs"
    >
      <span className="truncate font-medium text-foreground">{w.label}</span>
      <Meter w={w} pct={pct} />
      <span
        className={cn("text-right font-semibold tabular-nums", INK[usageLevel(pct)])}
      >
        {pct}%
      </span>
      <span className="truncate text-muted-foreground tabular-nums">
        {resetsIn(w, now) ?? (w.lastKnown ? "last known" : "")}
      </span>
    </li>
  );
}

/* one window as a bar beside the account's others, its name, percent and
   reset under it */
/* a tick between the bars once the windows stand side by side, centred on
   the bars' height: from 20rem of panel for two, 28rem for three */
const PARTED: Record<number, string> = {
  2: "@xs:not-first:before:absolute @xs:not-first:before:-top-[3px] @xs:not-first:before:h-3 @xs:not-first:before:-left-4 @xs:not-first:before:w-px @xs:not-first:before:bg-border",
  3: "@md:not-first:before:absolute @md:not-first:before:-top-[3px] @md:not-first:before:h-3 @md:not-first:before:-left-4 @md:not-first:before:w-px @md:not-first:before:bg-border",
};

function WindowBar({
  w,
  now,
  parted,
}: {
  w: UsageWindowView;
  now: number;
  parted: number;
}) {
  const pct = Math.round(w.usedPct);
  const when = resetsIn(w, now) ?? (w.lastKnown ? "last known" : null);
  return (
    <li
      title={windowDetail(w, now)}
      className={cn("relative grid min-w-0 gap-1.5", PARTED[parted])}
    >
      <Meter w={w} pct={pct} />
      <p className="flex min-w-0 items-baseline gap-1.5 text-xs whitespace-nowrap tabular-nums">
        {/* the window and how full it is read first; when it resets, after */}
        <span className="font-medium text-foreground">{w.label}</span>
        <span className={cn("font-semibold", INK[usageLevel(pct)])}>{pct}%</span>
        {when && <span className="truncate text-muted-foreground">· {when}</span>}
      </p>
    </li>
  );
}

const providerTitle = (view: AccountUsageView) =>
  Object.values(SUBSCRIPTION).find((entry) => entry.provider === view.provider)
    ?.title ??
  KINDS.find((entry) => entry.value === view.provider)?.label ??
  view.provider;

/** An account's closest window at a glance: its name, a ring and the percent. */
export function UsageSummary({ view }: { view?: AccountUsageView }) {
  const top = closestWindow(view);
  if (!top) return null;
  const pct = Math.round(top.usedPct);
  return (
    <span className="flex items-center gap-1.5 px-1 text-xs text-muted-foreground tabular-nums">
      <UsageRing pct={pct} />
      <span className={cn(usageLevel(pct) !== "ok" && INK[usageLevel(pct)])}>
        {top.label} {pct}%
      </span>
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
          <section key={label} aria-label={label} className="grid min-w-0 gap-1.5">
            {groups.size > 1 && (
              <p className="text-xs text-muted-foreground">{label}</p>
            )}
            {/* a few windows of one kind read as one meter, side by side;
                more, or several kinds, as rows */}
            {groups.size === 1 && windows.length <= 3 ? (
              <ul
                className={cn(
                  "grid min-w-0 gap-x-8 gap-y-3",
                  windows.length === 2 && "@xs:grid-cols-2",
                  windows.length === 3 && "@md:grid-cols-3",
                )}
              >
                {windows.map((w) => (
                  <WindowBar key={w.label} w={w} now={now} parted={windows.length} />
                ))}
              </ul>
            ) : (
              <ul className="grid min-w-0 grid-cols-[minmax(2.5rem,auto)_minmax(3rem,1fr)_3ch_minmax(0,auto)] gap-x-3 gap-y-1.5">
                {windows.map((w) => (
                  <WindowRow key={w.label} w={w} now={now} />
                ))}
              </ul>
            )}
          </section>
        ))
      ) : (
        <p className="text-xs text-muted-foreground">{view?.note ?? "Usage unknown"}</p>
      )}
      {read && (
        <p className="line-clamp-3 text-xs break-words text-muted-foreground">{read}</p>
      )}
    </>
  );
}

/* an account's place while the first read is out, in the shape most accounts
   take, so the panel doesn't jump when it lands */
function AccountSkeleton() {
  return (
    <div aria-hidden className="grid gap-2">
      <div className="flex items-center gap-1.5">
        <Skeleton className="size-3.5 rounded-full" />
        <Skeleton className="h-3 w-28" />
      </div>
      <div className="grid gap-x-8 gap-y-3 @xs:grid-cols-2">
        {[0, 1].map((n) => (
          <div key={n} className="grid gap-1.5">
            <Skeleton className="h-1.5 w-full rounded-full" />
            <Skeleton className="h-3 w-24" />
          </div>
        ))}
      </div>
    </div>
  );
}

export function AllProviderLimits({
  views,
  loading = false,
  onRefresh,
}: {
  views: readonly AccountUsageView[];
  /** the first read is still out: hold the panel's shape rather than say
      nothing was reported */
  loading?: boolean;
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
      aria-label="Usage limits"
      className="mb-6 grid gap-3 rounded-2xl bg-raised/50 px-4 pt-2.5 pb-3.5"
    >
      <div className="flex items-center justify-between gap-3">
        <h2 className="font-mono text-[11px] tracking-wide text-muted-foreground uppercase">
          Usage limits
        </h2>
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
      {loading && !views.length && (
        <div className="@container" aria-busy="true">
          <AccountSkeleton />
        </div>
      )}
      {!loading && !views.length && (
        <p className="text-sm text-muted-foreground">
          No account limits have been reported.
        </p>
      )}
      <div className="grid min-w-0 divide-y divide-border/60">
        {views.map((view, index) => (
          <div
            key={`${view.provider}:${view.label}:${index}`}
            className="@container grid min-w-0 gap-2 py-3 first:pt-0 last:pb-0"
          >
            <h3 className="flex min-w-0 items-center gap-1.5 text-xs font-medium text-muted-foreground">
              <ProviderLogo
                kind={
                  Object.entries(SUBSCRIPTION).find(
                    ([, entry]) => entry.provider === view.provider,
                  )?.[0] ?? view.provider
                }
                className="size-3.5"
              />
              <span className="min-w-0 truncate text-foreground">{view.label}</span>
              {providerTitle(view) !== view.label && (
                <span className="shrink-0 font-normal">{providerTitle(view)}</span>
              )}
            </h3>
            {view.read === "disabled" ? (
              <p className="text-xs text-muted-foreground">Disabled</p>
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
