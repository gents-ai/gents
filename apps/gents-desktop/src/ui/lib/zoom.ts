/* The page zooms the zoom keys step through, as a browser's do. */
export const ZOOM_STEPS = [0.67, 0.75, 0.8, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2] as const;

export type ZoomAction = "in" | "out" | "reset";

/** The zoom after `action`, from `level`: the next step up or down, or 1. */
export function stepZoom(level: number, action: ZoomAction): number {
  if (action === "reset") return 1;
  if (action === "in") return ZOOM_STEPS.find((step) => step > level) ?? level;
  return [...ZOOM_STEPS].reverse().find((step) => step < level) ?? level;
}

/** The step a key asks for: Ctrl with =, + (the keypad's too), − or 0. */
export function zoomKey(
  event: Pick<KeyboardEvent, "key" | "ctrlKey" | "metaKey" | "altKey">,
): ZoomAction | null {
  if (!event.ctrlKey || event.metaKey || event.altKey) return null;
  if (event.key === "=" || event.key === "+") return "in";
  if (event.key === "-") return "out";
  if (event.key === "0") return "reset";
  return null;
}

/** Calls `handle` with each step a key asks for. Windows and Linux have no
    View menu to carry the zoom keys, so the page takes them. */
export function onZoomKeys(target: Window, handle: (action: ZoomAction) => void) {
  const listen = (event: KeyboardEvent) => {
    const action = zoomKey(event);
    if (!action) return;
    event.preventDefault();
    handle(action);
  };
  /* capture: a field that stops its keys' propagation still zooms */
  target.addEventListener("keydown", listen, { capture: true });
  return () => target.removeEventListener("keydown", listen, { capture: true });
}
