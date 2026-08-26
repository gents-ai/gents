import { useEffect } from "react";

const HEIGHT = "--mobile-visual-viewport-height";
const WIDTH = "--mobile-visual-viewport-width";
const TOP = "--mobile-visual-viewport-top";
const LEFT = "--mobile-visual-viewport-left";
const MIN_USEFUL_DIMENSION = 64;

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
    return () => {
      if (frame != null) cancelAnimationFrame(frame);
      viewport.removeEventListener("resize", schedule);
      viewport.removeEventListener("scroll", schedule);
      window.removeEventListener("resize", schedule);
      window.removeEventListener("pageshow", schedule);
      window.removeEventListener("orientationchange", schedule);
      root.style.removeProperty(HEIGHT);
      root.style.removeProperty(WIDTH);
      root.style.removeProperty(TOP);
      root.style.removeProperty(LEFT);
    };
  }, []);
}
