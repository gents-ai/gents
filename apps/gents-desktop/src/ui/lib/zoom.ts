/* The page zooms the View menu steps through, as a browser's do. */
export const ZOOM_STEPS = [0.67, 0.75, 0.8, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2] as const;

export type ZoomAction = "in" | "out" | "reset";

/** The zoom after `action`, from `level`: the next step up or down, or 1. */
export function stepZoom(level: number, action: ZoomAction): number {
  if (action === "reset") return 1;
  if (action === "in") return ZOOM_STEPS.find((step) => step > level) ?? level;
  return [...ZOOM_STEPS].reverse().find((step) => step < level) ?? level;
}
