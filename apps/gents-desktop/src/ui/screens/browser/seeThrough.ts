/* The browser page is a native view beneath the app's webview (browser.rs).
   Where it shows, the app paints nothing: html, body and every ancestor of
   the placeholder that paints a background stop painting it, and a layer at
   the bottom of the page paints those same backgrounds back, each in its
   ancestor's own box, around a hole the shape of the page. Nothing of the
   app is masked or cut, so whatever it draws over the page (menus, the
   sidebar, dialogs, toasts) draws over it live.

   The app also reports what it draws over the page, so clicks there stay
   with the app; every other click in the hole goes to the page. */
import { invoke } from "@tauri-apps/api/core";
import { nextSeq } from "./order";

export type Rect = { x: number; y: number; width: number; height: number };

/* What the app draws over the page, found from where it lives rather than
   from a list of kinds: whatever is portalled to the body beside the app
   (menus, dialogs and their backdrops, popovers, tooltips, toasts), and
   whatever the app raises within itself with a z-index (the sidebar's hover
   panel and peek, banners). Something that takes no pointer events is
   skipped, so a click there still reaches the page. */
const LEFT_OUT = new Set(["SCRIPT", "STYLE", "LINK", "TEMPLATE"]);

function visibleBox(el: Element): DOMRect | null {
  const style = getComputedStyle(el);
  if (style.display === "none" || style.visibility === "hidden") return null;
  const box = el.getBoundingClientRect();
  return box.width > 0 && box.height > 0 ? box : null;
}

/* a portalled layer, or the parts of it that take the pointer when the
   layer itself lets it through, as popper wrappers do */
function layerBoxes(el: Element, depth: number, into: DOMRect[]) {
  const box = visibleBox(el);
  if (!box) return;
  if (getComputedStyle(el).pointerEvents !== "none") into.push(box);
  else if (depth > 0)
    for (const child of el.children) layerBoxes(child, depth - 1, into);
}

function overlayBoxes(
  placeholder: HTMLElement,
  appRoot: Element | null,
  own: Element[],
) {
  const boxes: DOMRect[] = [];
  for (const child of document.body.children)
    if (child !== appRoot && !own.includes(child) && !LEFT_OUT.has(child.tagName))
      layerBoxes(child, 2, boxes);
  const raised = appRoot?.querySelectorAll('[class*="z-"], [style*="z-index"]') ?? [];
  for (const el of raised) {
    if (el.contains(placeholder) || placeholder.contains(el)) continue;
    const style = getComputedStyle(el);
    if (style.position === "static" || !(parseInt(style.zIndex, 10) > 0)) continue;
    if (style.pointerEvents === "none") continue;
    const box = visibleBox(el);
    if (box) boxes.push(box);
  }
  return boxes;
}

const transparent = (color: string) =>
  color === "transparent" || /rgba?\(.*,\s*0\)$/.test(color);

const paints = (el: Element) => !transparent(getComputedStyle(el).backgroundColor);

/* ancestors that clip the placeholder: the page shows only where they do */
function clippers(el: HTMLElement) {
  const found: HTMLElement[] = [];
  for (let up = el.parentElement; up; up = up.parentElement) {
    const style = getComputedStyle(up);
    if (style.overflowX !== "visible" || style.overflowY !== "visible") found.push(up);
  }
  return found;
}

/* the inner radius of the nearest rounded ancestor: the panel's bottom
   corners, which the page takes on so the panel's border sits around it */
function cornerOf(el: HTMLElement) {
  for (let up = el.parentElement; up; up = up.parentElement) {
    const style = getComputedStyle(up);
    const radius = parseFloat(style.borderBottomLeftRadius);
    if (radius > 0) return Math.max(0, radius - parseFloat(style.borderBottomWidth));
  }
  return 0;
}

function measure(el: HTMLElement, clips: HTMLElement[]) {
  const box = el.getBoundingClientRect();
  let left = Math.max(box.left, 0);
  let top = Math.max(box.top, 0);
  let right = Math.min(box.right, window.innerWidth);
  let bottom = Math.min(box.bottom, window.innerHeight);
  for (const up of clips) {
    const clip = up.getBoundingClientRect();
    left = Math.max(left, clip.left);
    top = Math.max(top, clip.top);
    right = Math.min(right, clip.right);
    bottom = Math.min(bottom, clip.bottom);
  }
  return {
    bounds: { x: box.left, y: box.top, width: box.width, height: box.height },
    open: {
      x: left,
      y: top,
      width: Math.max(0, right - left),
      height: Math.max(0, bottom - top),
    },
  };
}

const crosses = (a: Rect, b: DOMRect) =>
  b.width > 0 &&
  b.height > 0 &&
  b.right > a.x &&
  b.bottom > a.y &&
  b.left < a.x + a.width &&
  b.top < a.y + a.height;

type Painter = { el: HTMLElement; color: string; saved: string };

/** Keep the native page beneath `el` while the effect runs. */
export function seeThrough(el: HTMLElement): () => void {
  const root = document.documentElement;
  const body = document.body;
  const clips = clippers(el);
  const radius = cornerOf(el);

  /* the app's own tree, and what this module adds beside it */
  const appRoot = document.getElementById("root");
  let painters: Painter[] = [];
  let base = "";
  const saved = { html: root.style.background, body: body.style.background };
  const underlay = document.createElement("div");
  underlay.dataset.browserUnderlay = "";
  underlay.style.cssText =
    "position:fixed;inset:0;z-index:-1;pointer-events:none;overflow:hidden";
  /* plugin pages' frames are laid out in the app's page, never over a panel */
  const own = [underlay, document.getElementById("ux-frames")].filter(
    (el): el is HTMLElement => el !== null,
  );

  /* each painting ancestor, outermost first, and the page's own colour */
  const snapshot = () => {
    const found: HTMLElement[] = [];
    for (let up = el.parentElement; up && up !== body; up = up.parentElement)
      if (paints(up)) found.push(up);
    painters = found.reverse().map((up) => ({
      el: up,
      color: getComputedStyle(up).backgroundColor,
      saved: up.style.backgroundColor,
    }));
    const bodyColor = getComputedStyle(body).backgroundColor;
    base = transparent(bodyColor) ? getComputedStyle(root).backgroundColor : bodyColor;
  };

  const lift = () => {
    root.style.background = "transparent";
    body.style.background = "transparent";
    for (const p of painters) p.el.style.backgroundColor = "transparent";
    body.prepend(underlay);
  };
  const restore = () => {
    root.style.background = saved.html;
    body.style.background = saved.body;
    for (const p of painters) p.el.style.backgroundColor = p.saved;
    underlay.remove();
  };

  /* one layer per painter: its box, its corners, its colour everywhere in
     it but the hole; the hole's own corners round with the page's */
  const layer = (box: DOMRect | null, boxRadius: string, color: string, hole: Rect) => {
    const outer = document.createElement("div");
    const at = box ?? new DOMRect(0, 0, window.innerWidth, window.innerHeight);
    outer.style.cssText = `position:fixed;left:${at.left}px;top:${at.top}px;width:${at.width}px;height:${at.height}px;border-radius:${boxRadius};overflow:hidden`;
    const ring = document.createElement("div");
    ring.style.cssText = `position:absolute;left:${hole.x - at.left}px;top:${hole.y - at.top}px;width:${hole.width}px;height:${hole.height}px;border-radius:0 0 ${radius}px ${radius}px;box-shadow:0 0 0 200vmax ${color}`;
    outer.appendChild(ring);
    return outer;
  };

  const paint = (hole: Rect) => {
    const layers = [layer(null, "0", base, hole)];
    for (const p of painters)
      layers.push(
        layer(
          p.el.getBoundingClientRect(),
          getComputedStyle(p.el).borderRadius,
          p.color,
          hole,
        ),
      );
    underlay.replaceChildren(...layers);
  };

  /* a theme change recolours the painters */
  const theme = new MutationObserver(() => {
    restore();
    snapshot();
    lift();
    sent = "";
  });

  let sent = "";
  let frame = 0;
  let shown = false;

  const tick = () => {
    frame = requestAnimationFrame(tick);
    const { bounds, open } = measure(el, clips);
    const visible = open.width >= 2 && open.height >= 2;
    const solids = visible
      ? overlayBoxes(el, appRoot, own)
          .filter((r) => crosses(open, r))
          .map((r) => ({ x: r.left, y: r.top, width: r.width, height: r.height }))
      : [];
    const next = visible ? JSON.stringify([bounds, open, solids]) : "hidden";
    if (next === sent) return;
    sent = next;
    if (!visible) {
      if (shown) {
        shown = false;
        restore();
        void invoke("browser_hide", { seq: nextSeq() }).catch(() => {});
      }
      return;
    }
    if (!shown) {
      shown = true;
      lift();
    }
    paint(open);
    void invoke("browser_place", {
      placement: { bounds, open, solids, radius },
      seq: nextSeq(),
    }).catch(() => {});
  };

  snapshot();
  theme.observe(root, { attributes: true, attributeFilter: ["class", "data-theme"] });
  tick();

  return () => {
    cancelAnimationFrame(frame);
    theme.disconnect();
    if (shown) restore();
    void invoke("browser_hide", { seq: nextSeq() }).catch(() => {});
  };
}
