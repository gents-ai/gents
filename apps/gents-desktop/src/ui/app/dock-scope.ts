/* Which of the dock's tabs apply on a route, and which one shows there.
   A tab out of scope is kept, not dropped: it is back when the person
   returns to where it belongs. */
import type { Placement, Surface } from "./surfaces";
import type { DockState } from "./workspace";

export const inScope = (s: Surface, routeName: string) =>
  !s.routes || s.routes.includes(routeName);

/** The dock on a route, from `surfaces`, every one registered. */
export function dockView(
  dock: DockState,
  routeName: string,
  surfaces: readonly Surface[],
  placement: Placement = "dock",
) {
  const placed = surfaces.filter((s) => s.placements.includes(placement));
  const tabs = dock.tabs
    .map((id) => placed.find((s) => s.id === id))
    .filter((s): s is Surface => s !== undefined && inScope(s, routeName));
  const active = tabs.find((s) => s.id === dock.active) ?? tabs[0] ?? null;
  const shown = dock.open && active !== null;
  const available = placed.filter(
    (s) => inScope(s, routeName) && !dock.tabs.includes(s.id),
  );
  return { tabs, active, shown, available };
}
