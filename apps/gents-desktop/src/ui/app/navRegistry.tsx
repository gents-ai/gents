/* The side nav's items as data. The rail (icon + tooltip) and the panel
   (icon + label) both read one list, so an item is declared once and the two
   presentations cannot drift. A feature registers its row next to its screen
   and gets a disposer back.

   The list is the `nav` area of the contribution registry: this module is a
   typed wrapper over it, so a UX plugin's ctx.register({ area: NAV_AREA })
   and the app's own registerNavItem land in the same place. The snapshot is
   referentially stable until a mutation so useSyncExternalStore does not
   loop. */
import { useSyncExternalStore, type ReactNode } from "react";
import { Inbox, Plus, ScrollText, Users } from "lucide-react";
import type { Route } from "@/lib/router";
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
  /** primary rows sit on the rail and at the top of the panel;
      footer rows sit at the panel's foot only */
  placement: "primary" | "footer";
  /** ascending within a placement; ties keep registration order */
  order?: number;
}

function isNavItem(data: unknown): data is NavItem {
  return (
    typeof data === "object" &&
    data !== null &&
    typeof (data as NavItem).id === "string" &&
    typeof (data as NavItem).active === "function" &&
    typeof (data as NavItem).placement === "string"
  );
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
   rather than crashing the rail */
export function navItems(): readonly NavItem[] {
  const area = registry.getArea(NAV_AREA);
  if (area !== snapshotFor) {
    snapshotFor = area;
    snapshot = area.map((c) => c.data).filter(isNavItem);
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

/* the app's own rows; a feature elsewhere registers the same way */
registerNavItem({
  id: "new-session",
  label: "New session",
  icon: <Plus className="size-4" />,
  to: { name: "session", sessionId: null },
  active: (route) => route.name === "session" && route.sessionId === null,
  placement: "primary",
  order: 10,
});

registerNavItem({
  id: "mailbox",
  label: "Mailbox",
  icon: <Inbox className="size-4" />,
  to: { name: "mailbox" },
  active: (route) => route.name === "mailbox",
  count: (ctx) => ctx.mailboxCount,
  placement: "primary",
  order: 20,
});

registerNavItem({
  id: "sessions",
  label: "Sessions",
  icon: <ScrollText className="size-4" />,
  to: { name: "sessions" },
  active: (route) =>
    route.name === "sessions" || (route.name === "session" && route.sessionId !== null),
  placement: "primary",
  order: 30,
});

registerNavItem({
  id: "agents",
  label: "Agents",
  icon: <Users className="size-4" />,
  to: { name: "agents" },
  active: (route) => route.name === "agents",
  placement: "footer",
  order: 10,
});
