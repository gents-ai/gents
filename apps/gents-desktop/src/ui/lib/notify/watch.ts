import type { FleetStore } from "../../../hooks/fleetStore";
import type { SelectionStore } from "../../../hooks/selectionStore";
import { toast } from "sonner";
import { flag, notifyKinds } from "@/preferences";
import { navigate, type Route } from "../router";
import { changesBetween, type NotifyEvent, type NotifyKind } from "./events";
import { notifyPlatform, type Notice, type NotifyPlatform } from "./platform";

/* what arrives together is told together: a quiet gap closes a burst, and
   a burst that keeps going is told after a while anyway */
export const BURST_QUIET = 2500;
export const BURST_MOST = 6000;
/* without a click callback, coming back to the app this soon after a
   notification offers what it was about; the app never goes there unasked */
export const FOLLOW_WITHIN = 20_000;

/** What a notification offers once the window is back: its own words, and
    Open for one thing or Show for several. */
export type Offer = { title: string; action: "Open" | "Show"; to: Route };

export type NotifyDeps = {
  platform: NotifyPlatform;
  now: () => number;
  /** the window is in front and shown */
  focused: () => boolean;
  /** the window is shown, in front or not */
  visible: () => boolean;
  enabled: () => boolean;
  kinds: () => Record<NotifyKind, boolean>;
  open: (to: Route) => void;
  /** offers to open what a notification was about, in the app */
  offer: (offer: Offer, open: () => void) => void;
  /** calls `then` each time the window comes back to the front */
  onFocus: (then: () => void) => () => void;
};

const defaults = (): NotifyDeps => ({
  platform: notifyPlatform,
  now: () => Date.now(),
  focused: () => document.visibilityState === "visible" && document.hasFocus(),
  visible: () => document.visibilityState === "visible",
  enabled: () => flag("notifications"),
  kinds: notifyKinds,
  open: navigate,
  offer: (offer, open) =>
    void toast(offer.title, { action: { label: offer.action, onClick: open } }),
  onFocus: (then) => {
    if (typeof window === "undefined") return () => {};
    window.addEventListener("focus", then);
    return () => window.removeEventListener("focus", then);
  },
});

const COUNTED: Record<NotifyKind, (n: number) => string> = {
  finished: (n) => `${n} sessions finished`,
  failed: (n) => `${n} sessions failed`,
  question: (n) => `${n} questions need your answer`,
  mailbox: (n) => `${n} new items in the mailbox`,
};

const ONE: Record<NotifyKind, (e: NotifyEvent) => string> = {
  finished: (e) => `Finished: ${e.title}`,
  failed: (e) => (e.worker ? `Worker failed: ${e.title}` : `Failed: ${e.title}`),
  question: (e) => `Needs your answer: ${e.title}`,
  mailbox: (e) => `In your mailbox: ${e.title}`,
};

const MANY_LEADS_TO: Record<NotifyKind, Route> = {
  finished: { name: "sessions" },
  failed: { name: "sessions" },
  question: { name: "mailbox" },
  mailbox: { name: "mailbox" },
};

const listed = (titles: string[]) =>
  titles.length > 3
    ? `${titles.slice(0, 3).join(", ")}, and ${titles.length - 3} more`
    : titles.join(", ");

/** One notification per kind for what arrived together. */
export function noticesFor(
  events: readonly NotifyEvent[],
): (Notice & { to: Route; many: boolean })[] {
  const byKind = new Map<NotifyKind, NotifyEvent[]>();
  for (const e of events) byKind.set(e.kind, [...(byKind.get(e.kind) ?? []), e]);
  return [...byKind].map(([kind, told]) =>
    told.length === 1
      ? { title: ONE[kind](told[0]!), to: told[0]!.to, many: false }
      : {
          title: COUNTED[kind](told.length),
          body: listed(told.map((e) => e.title)),
          to: MANY_LEADS_TO[kind],
          many: true,
        },
  );
}

/** The app's one owner of system notifications. It watches the fleet for
    what finished, failed, asked or arrived after it started, and tells the
    person while the window is in the background: never about the session
    on screen, never twice for one request or item, a burst as one notice
    per kind, each kind as this device has it switched. */
export function watchNotifications(
  stores: { fleet: FleetStore; selection: SelectionStore },
  deps: NotifyDeps = defaults(),
) {
  const since = new Date(deps.now()).toISOString();
  const told = new Set<string>();
  let prev = stores.fleet.getState();
  let queued: NotifyEvent[] = [];
  let first = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pending: (Offer & { at: number })[] = [];

  const onScreen = (e: NotifyEvent) => {
    if (!e.session || !deps.visible()) return false;
    const { nodeDid, sessionId } = stores.selection.getState();
    return e.session.nodeDid === nodeDid && e.session.sessionId === sessionId;
  };

  const flush = () => {
    timer = null;
    const events = queued;
    queued = [];
    if (deps.focused()) return;
    for (const notice of noticesFor(events)) {
      deps.platform.show({
        title: notice.title,
        body: notice.body,
        onClick: () => {
          pending = [];
          deps.open(notice.to);
        },
      });
      pending.push({
        title: notice.title,
        action: notice.many ? "Show" : "Open",
        to: notice.to,
        at: deps.now(),
      });
    }
  };

  const unsubscribe = stores.fleet.subscribe((next) => {
    const events = changesBetween(prev, next, since);
    prev = next;
    if (!deps.enabled()) return;
    const kinds = deps.kinds();
    for (const e of events) {
      if (!kinds[e.kind] || told.has(e.key)) continue;
      told.add(e.key);
      if (deps.focused() || onScreen(e)) continue;
      if (queued.length === 0) first = deps.now();
      queued.push(e);
    }
    if (queued.length === 0) return;
    if (timer) clearTimeout(timer);
    const left = BURST_MOST - (deps.now() - first);
    timer = setTimeout(flush, Math.max(0, Math.min(BURST_QUIET, left)));
  });

  const followed = deps.onFocus(() => {
    if (deps.platform.reportsClicks || !pending.length) return;
    const recent = pending.filter((p) => deps.now() - p.at <= FOLLOW_WITHIN);
    pending = [];
    for (const { at: _at, ...offer } of recent)
      deps.offer(offer, () => deps.open(offer.to));
  });

  return () => {
    unsubscribe();
    followed();
    if (timer) clearTimeout(timer);
  };
}
