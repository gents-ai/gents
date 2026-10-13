/* The side nav's contributed rows: what a UX plugin adds through the `nav`
   area. The app's own rows (New session, Mailbox, Sessions, Nodes) are
   placed by hand in Rail and NavPanel; this list holds everything added
   after them. The rail (icon + tooltip) and the panel (icon + label) both
   read it, so a row is declared once and the two presentations cannot
   drift. A plugin registers through ctx.register({ area: NAV_AREA }); the
   app can register the same way with registerNavItem. The snapshot is
   referentially stable until a mutation so useSyncExternalStore does not
   loop. */
import { useSyncExternalStore, type ReactNode } from "react";
import { pathFor, type Route } from "@/lib/router";
import { registry } from "@/contrib/registry";
import { NAV_AREA, type Contribution } from "@/contrib/types";

/** what an item's badge may be derived from; extend as badge sources appear */
export interface NavContext {
  mailboxCount: number;
}

export interface NavItem {
  /** stable, unique; re-registering an id replaces the earlier entry */
  id: string;
  label: string;
  icon: ReactNode;
  to: Route;
  /** whether the row is the current place; a row may own several route shapes */
  active: (route: Route) => boolean;
  /** a count badge; omit or return 0 for none */
  count?: (ctx: NavContext) => number | undefined;
  /** primary rows sit on the rail and at the top of the panel, after the
      app's own; footer rows sit at the panel's foot only, before Nodes */
  placement: "primary" | "footer";
  /** ascending within a placement; ties keep registration order */
  order?: number;
}

/* the whole payload, so one bad row is dropped here rather than thrown
   from the Rail: a label, a route pathFor can print, a predicate, a
   placement, and a badge that is a function if it is anything */
function isNavItem(data: unknown): data is NavItem {
  if (typeof data !== "object" || data === null) return false;
  const item = data as Partial<NavItem>;
  if (typeof item.label !== "string" || item.label.length === 0) return false;
  if (typeof item.active !== "function") return false;
  if (item.placement !== "primary" && item.placement !== "footer") return false;
  if (item.count !== undefined && typeof item.count !== "function") return false;
  if (
    typeof item.to !== "object" ||
    item.to === null ||
    typeof item.to.name !== "string"
  )
    return false;
  try {
    return typeof pathFor(item.to) === "string";
  } catch {
    return false;
  }
}

/** a row's live state, with the plugin's predicates kept to themselves:
    a throw reads as inactive / no badge, and is logged once per call */
export function navRowState(
  item: NavItem,
  route: Route,
  ctx: NavContext,
): { active: boolean; count: number | undefined } {
  let active = false;
  let count: number | undefined;
  try {
    active = item.active(route) === true;
  } catch (error) {
    console.error(`[ux-plugins] nav:${item.id} active() threw`, error);
  }
  try {
    const value = item.count?.(ctx);
    if (typeof value === "number" && Number.isFinite(value)) count = value;
  } catch (error) {
    console.error(`[ux-plugins] nav:${item.id} count() threw`, error);
  }
  return { active, count };
}

export function registerNavItem(item: NavItem): () => void {
  return registry.register({
    id: item.id,
    area: NAV_AREA,
    order: item.order,
    data: item,
  });
}

let snapshotFor: readonly Contribution[] | null = null;
let snapshot: readonly NavItem[] = [];

/* the registry caches per area; the nav keeps its own derived list keyed on
   that reference, so a plugin's row with a malformed payload is dropped
   rather than crashing the rail. A row's id is the registry's namespaced
   contribution id (`<plugin>:<local>`), unique by construction, whatever
   the payload says: React keys on it. */
export function navItems(): readonly NavItem[] {
  const area = registry.getArea(NAV_AREA);
  if (area !== snapshotFor) {
    snapshotFor = area;
    snapshot = area.flatMap((c) =>
      isNavItem(c.data) ? [{ ...c.data, id: c.id }] : [],
    );
  }
  return snapshot;
}

function subscribe(fn: () => void) {
  return registry.subscribeArea(NAV_AREA, fn);
}

export function useNavItems(placement?: NavItem["placement"]) {
  const all = useSyncExternalStore(subscribe, navItems, navItems);
  return placement ? all.filter((item) => item.placement === placement) : all;
}
