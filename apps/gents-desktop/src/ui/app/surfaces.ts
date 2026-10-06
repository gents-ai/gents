/* Surfaces: the units the workspace can show beside the main pane. Each
   is registered — an id, a title, a mark, where it may be placed — and the
   dock finds it by id. A new surface is one file and one entry; the dock
   does not change. The gents chrome (rail, nav panel) never
   hosts a surface: that space is the app's own. */
import type { ComponentType } from "react";
import type { FleetState } from "../../hooks/fleetStore";
import { createRegistry } from "./registry";

/** dock: the right column. inline: a block inside the main pane's flow.
    sheet: a phone, where the dock does not exist. */
export type Placement = "dock" | "inline" | "sheet";

export type SurfaceContext = {
  placement: Placement;
  /** the session in the main pane, when there is one */
  sessionId: string | null;
};

export type Surface = {
  id: string;
  title: string;
  icon: ComponentType<{ className?: string }>;
  placements: readonly Placement[];
  /** route names the surface belongs to; absent means every route */
  routes?: readonly string[];
  render: ComponentType<SurfaceContext>;
  /** a small count for the tab, from the fleet; null shows nothing */
  badge?: (fleet: FleetState, sessionId: string | null) => number | null;
};

const registry = createRegistry<Surface>();

export const registerSurface = registry.register;
export const getSurface = registry.get;
export const listSurfaces = (placement?: Placement): Surface[] =>
  registry.list().filter((s) => !placement || s.placements.includes(placement));
/** Every registered surface; re-renders when one is added or removed. */
export const useSurfaces = registry.useList;

/** tests only */
export const clearSurfaces = registry.clear;
