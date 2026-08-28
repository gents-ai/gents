import { useEffect } from "react";

const HEIGHT = "--mobile-visual-viewport-height";
const WIDTH = "--mobile-visual-viewport-width";
const TOP = "--mobile-visual-viewport-top";
const LEFT = "--mobile-visual-viewport-left";
const MIN_USEFUL_DIMENSION = 64;
const FOCUSED_CONTROL_SELECTOR =
  'input:not([type="hidden"]):not([disabled]), textarea:not([disabled]), select:not([disabled]), [contenteditable="true"]';
const SCROLL_OWNER_SELECTOR = "[data-scroll-owner]";
const FOCUSED_CONTROL_MARGIN = 16;
const KEYBOARD_SETTLE_DELAY_MS = 350;

function revealFocusedControl(viewport: VisualViewport) {
  if (
    viewport.width > 760 ||
    viewport.width < MIN_USEFUL_DIMENSION ||
    viewport.height < MIN_USEFUL_DIMENSION
  ) {
    return;
  }
  const control = document.activeElement;
  if (!(control instanceof HTMLElement) || !control.matches(FOCUSED_CONTROL_SELECTOR)) {
    return;
  }
  const owner = control.closest<HTMLElement>(SCROLL_OWNER_SELECTOR);
  if (!owner) return;

  const rect = control.getBoundingClientRect();
  const ownerRect = owner.getBoundingClientRect();
  const viewportTop = Math.max(0, viewport.offsetTop);
  const viewportBottom = viewportTop + viewport.height;
  const visibleTop = Math.max(viewportTop, ownerRect.top) + FOCUSED_CONTROL_MARGIN;
  const visibleBottom =
    Math.min(viewportBottom, ownerRect.bottom) - FOCUSED_CONTROL_MARGIN;
  if (visibleBottom <= visibleTop) return;
  if (rect.top >= visibleTop && rect.bottom <= visibleBottom) return;

  owner.scrollTop +=
    rect.top < visibleTop ? rect.top - visibleTop : rect.bottom - visibleBottom;
}

/**
 * Keep the mobile shell pinned to WebKit's visible viewport while the software
 * keyboard or browser chrome changes it. CSS dynamic viewport units can lag a
 * keyboard dismissal in WKWebView, leaving the title above the visible area
 * and a phantom gap below the composer.
 */
export function useMobileVisualViewport() {
  useEffect(() => {
    const viewport = window.visualViewport;
    if (!viewport) return;

    const root = document.documentElement;
    let frame: number | null = null;
    let revealFrame: number | null = null;
    let settleTimer: ReturnType<typeof setTimeout> | null = null;
    const scheduleReveal = () => {
      if (revealFrame == null) {
        revealFrame = requestAnimationFrame(() => {
          revealFrame = null;
          revealFocusedControl(viewport);
        });
      }
      if (settleTimer != null) clearTimeout(settleTimer);
      settleTimer = setTimeout(() => {
        settleTimer = null;
        revealFocusedControl(viewport);
      }, KEYBOARD_SETTLE_DELAY_MS);
    };
    const apply = () => {
      frame = null;
      if (
        !Number.isFinite(viewport.height) ||
        !Number.isFinite(viewport.width) ||
        viewport.height < MIN_USEFUL_DIMENSION ||
        viewport.width < MIN_USEFUL_DIMENSION
      ) {
        root.style.removeProperty(HEIGHT);
        root.style.removeProperty(WIDTH);
        root.style.removeProperty(TOP);
        root.style.removeProperty(LEFT);
        return;
      }
      root.style.setProperty(HEIGHT, `${viewport.height}px`);
      root.style.setProperty(WIDTH, `${viewport.width}px`);
      root.style.setProperty(TOP, `${Math.max(0, viewport.offsetTop)}px`);
      root.style.setProperty(LEFT, `${Math.max(0, viewport.offsetLeft)}px`);
      scheduleReveal();
    };
    const schedule = () => {
      if (frame == null) frame = requestAnimationFrame(apply);
    };

    apply();
    schedule();
    viewport.addEventListener("resize", schedule);
    viewport.addEventListener("scroll", schedule);
    window.addEventListener("resize", schedule);
    window.addEventListener("pageshow", schedule);
    window.addEventListener("orientationchange", schedule);
    document.addEventListener("focusin", scheduleReveal);
    return () => {
      if (frame != null) cancelAnimationFrame(frame);
      if (revealFrame != null) cancelAnimationFrame(revealFrame);
      if (settleTimer != null) clearTimeout(settleTimer);
      viewport.removeEventListener("resize", schedule);
      viewport.removeEventListener("scroll", schedule);
      window.removeEventListener("resize", schedule);
      window.removeEventListener("pageshow", schedule);
      window.removeEventListener("orientationchange", schedule);
      document.removeEventListener("focusin", scheduleReveal);
      root.style.removeProperty(HEIGHT);
      root.style.removeProperty(WIDTH);
      root.style.removeProperty(TOP);
      root.style.removeProperty(LEFT);
    };
  }, []);
}
