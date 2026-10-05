/* Surfaces: the units the workspace can show beside the main pane. Each
   is registered once — an id, a title, a mark, where it may be placed —
   and the shell finds it by id. A new surface is one file and one entry;
   the shell does not change. The gents chrome (rail, nav panel) never
   hosts a surface: that space is the app's own. */
import type { ComponentType } from "react";
import type { DeploymentView } from "@source-inc/gents-desktop-client";

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
  /** a small count for the tab, from the shell's nodes; null shows nothing */
  badge?: (nodes: DeploymentView[], sessionId: string | null) => number | null;
};

const registry = new Map<string, Surface>();

export function registerSurface(surface: Surface): Surface {
  registry.set(surface.id, surface);
  return surface;
}

export const getSurface = (id: string | null): Surface | null =>
  id ? (registry.get(id) ?? null) : null;

export const listSurfaces = (placement?: Placement): Surface[] =>
  [...registry.values()].filter((s) => !placement || s.placements.includes(placement));

/** tests only */
export const clearSurfaces = () => registry.clear();
