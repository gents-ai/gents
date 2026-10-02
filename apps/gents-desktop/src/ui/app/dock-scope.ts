/* Which of the dock's tabs apply on a route, and which one shows there.
   A tab out of scope is kept, not dropped: it is back when the person
   returns to where it belongs. */
import { getSurface, listSurfaces, type Placement, type Surface } from "./surfaces";
import type { DockState } from "./workspace";

export const inScope = (s: Surface, routeName: string) =>
  !s.routes || s.routes.includes(routeName);

export function dockView(
  dock: DockState,
  routeName: string,
  placement: Placement = "dock",
) {
  const tabs = dock.tabs
    .map((id) => getSurface(id))
    .filter(
      (s): s is Surface =>
        s !== null && s.placements.includes(placement) && inScope(s, routeName),
    );
  const active = tabs.find((s) => s.id === dock.active) ?? tabs[0] ?? null;
  const shown = dock.open && active !== null;
  const available = listSurfaces(placement).filter(
    (s) => inScope(s, routeName) && !dock.tabs.includes(s.id),
  );
  return { tabs, active, shown, available };
}
