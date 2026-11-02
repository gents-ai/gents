import { useEffect } from "react";
import { bindVisualViewport } from "@gents/shell";
import { scrollParent } from "@gents/ui/conversation";

const MIN_USEFUL_DIMENSION = 64;
const FOCUSED_CONTROL_SELECTOR =
  'input:not([type="hidden"]):not([disabled]), textarea:not([disabled]), select:not([disabled]), [contenteditable="true"]';
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
  const owner = scrollParent(control);
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

export function useMobileVisualViewport() {
  useEffect(() => {
    const viewport = window.visualViewport;
    if (!viewport) return;

    let followFocus = true;
    let revealFrame: number | null = null;
    let settleTimer: ReturnType<typeof setTimeout> | null = null;
    const cancelReveal = () => {
      followFocus = false;
      if (revealFrame != null) cancelAnimationFrame(revealFrame);
      if (settleTimer != null) clearTimeout(settleTimer);
      revealFrame = null;
      settleTimer = null;
    };
    const scheduleReveal = () => {
      if (!followFocus) return;
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
    const onFocus = () => {
      followFocus = true;
      scheduleReveal();
    };
    const unbindViewport = bindVisualViewport(document.documentElement, scheduleReveal);
    document.addEventListener("focusin", onFocus);
    document.addEventListener("touchmove", cancelReveal, { passive: true });
    document.addEventListener("wheel", cancelReveal, { passive: true });
    return () => {
      unbindViewport();
      cancelReveal();
      document.removeEventListener("focusin", onFocus);
      document.removeEventListener("touchmove", cancelReveal);
      document.removeEventListener("wheel", cancelReveal);
    };
  }, []);
}
