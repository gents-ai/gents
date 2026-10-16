import type { ComponentType } from "react";
import type { FleetState } from "../../hooks/fleetStore";
import { createRegistry } from "./registry";

export type Placement = "dock" | "inline" | "sheet";

export type SurfaceContext = {
  placement: Placement;
  sessionId: string | null;
};

export type Surface = {
  id: string;
  title: string;
  icon: ComponentType<{ className?: string }>;
  placements: readonly Placement[];
  routes?: readonly string[];
  render: ComponentType<SurfaceContext>;
  badge?: (fleet: FleetState, sessionId: string | null) => number | null;
};

const registry = createRegistry<Surface>();

export const registerSurface = registry.register;
export const getSurface = registry.get;
export const listSurfaces = (placement?: Placement): Surface[] =>
  registry.list().filter((s) => !placement || s.placements.includes(placement));
export const useSurfaces = registry.useList;
// Tests only.
export const clearSurfaces = registry.clear;
