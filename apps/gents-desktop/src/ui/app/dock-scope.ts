import type { Placement, Surface } from "./surfaces";
import type { DockState } from "./workspace";

export const inScope = (s: Surface, routeName: string) =>
  !s.routes || s.routes.includes(routeName);

/** What a dock shows on a page: its tabs that belong there, the one showing,
    and the panels it could still open. An open dock with no tab here shows
    those panels instead, so it is shown whenever it has either. */
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
  const available = placed.filter(
    (s) => inScope(s, routeName) && !dock.tabs.includes(s.id),
  );
  const shown = dock.open && (active !== null || available.length > 0);
  return { tabs, active, shown, available };
}
