/* Where plugin frames live. A frame reloads when it is moved in the DOM,
   so no frame ever moves: every one sits in a fixed layer under the shell's
   overlays and is positioned over the viewport that shows its page
   (resize and scroll re-measure it), and hidden with no viewport. One
   layer, one frame per plugin, no reparenting. */

const STAGE_ID = "ux-frames";

export function stageElement(): HTMLElement {
  let stage = document.getElementById(STAGE_ID);
  if (stage) return stage;
  stage = document.createElement("div");
  stage.id = STAGE_ID;
  Object.assign(stage.style, {
    position: "fixed",
    inset: "0",
    pointerEvents: "none",
    zIndex: "1",
    width: "0",
    height: "0",
    overflow: "visible",
  });
  document.body.appendChild(stage);
  return stage;
}

interface Placement {
  frame: HTMLIFrameElement;
  anchor: HTMLElement;
  observer: ResizeObserver | null;
}

const placements = new Map<string, Placement>();
let listening = false;

function hide(frame: HTMLIFrameElement) {
  Object.assign(frame.style, {
    position: "fixed",
    left: "0",
    top: "0",
    width: "0",
    height: "0",
    border: "0",
    visibility: "hidden",
    pointerEvents: "none",
  });
}

function sync(placement: Placement) {
  const rect = placement.anchor.getBoundingClientRect();
  Object.assign(placement.frame.style, {
    position: "fixed",
    left: `${rect.left}px`,
    top: `${rect.top}px`,
    width: `${rect.width}px`,
    height: `${rect.height}px`,
    border: "0",
    visibility: rect.width > 0 && rect.height > 0 ? "visible" : "hidden",
    pointerEvents: "auto",
  });
}

function syncAll() {
  for (const placement of placements.values()) sync(placement);
}

function listen() {
  if (listening || typeof window === "undefined") return;
  listening = true;
  window.addEventListener("resize", syncAll);
  document.addEventListener("scroll", syncAll, { capture: true, passive: true });
}

/** show `frame` over `anchor`, or hide it when there is no anchor */
export function placeFrame(
  key: string,
  frame: HTMLIFrameElement | null,
  anchor: HTMLElement | null,
): void {
  const previous = placements.get(key);
  previous?.observer?.disconnect();
  placements.delete(key);
  if (!frame) return;
  if (!anchor) {
    hide(frame);
    return;
  }
  listen();
  const placement: Placement = {
    frame,
    anchor,
    observer:
      typeof ResizeObserver !== "undefined"
        ? new ResizeObserver(() => sync(placement))
        : null,
  };
  placement.observer?.observe(anchor);
  placements.set(key, placement);
  sync(placement);
}

/** test seam: the anchor a frame is placed over */
export function placementOf(key: string): HTMLElement | null {
  return placements.get(key)?.anchor ?? null;
}
