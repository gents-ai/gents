// macOS two-finger history swipes, recognised natively and reported as
// progress in [-1, 1] (positive is back). AppKit's tracker takes over the
// scroll stream once it starts, so the webview keeps native told whether
// the content under the pointer can still scroll sideways: content scrolls
// first and the swipe arms at its edge.
import { useEffect, useRef } from "react";
import { listenToWindow, reportSwipeScrollEdges } from "../../lib/nativeShell";
import { isMacTauriShell } from "../../lib/shellPlatform";
import type { History } from "./router";
import { SWIPE_HANDLE_ATTR } from "../app/SwipeHandles";
import { scrollRoom } from "./scrollRoom";

type SwipeProgress = { amount: number; phase: "moving" | "ended" | "cancelled" };

export const COMMIT = 0.4;
const TRAVEL_PX = 96;

const handle = (direction: "back" | "forward") =>
  document.querySelector<HTMLElement>(`[${SWIPE_HANDLE_ATTR}="${direction}"]`);

function show(el: HTMLElement, progress: number, sign: 1 | -1) {
  const p = Math.min(progress / COMMIT, 1);
  el.style.transition = "";
  el.style.transform = `translate(${sign * (p * TRAVEL_PX - 40)}px, -50%)`;
  el.style.opacity = String(0.6 + p * 0.4);
  const fill = el.querySelector<HTMLElement>("[data-swipe-fill]");
  if (fill) fill.style.opacity = (p * p).toFixed(3);
  if (p >= 1) el.dataset.armed = "";
  else delete el.dataset.armed;
}

function hide(el: HTMLElement) {
  el.style.transition = "transform 200ms ease-out, opacity 200ms ease-out";
  el.style.transform = "";
  el.style.opacity = "";
  el.querySelector<HTMLElement>("[data-swipe-fill]")?.style.removeProperty("opacity");
  delete el.dataset.armed;
}

function reportScrollRoom(): () => void {
  let x = -1;
  let y = -1;
  let frame = 0;
  let sent = "";
  let dead = false;
  const measure = () => {
    frame = 0;
    if (dead || x < 0) return;
    const { left, right } = scrollRoom(document.elementFromPoint(x, y));
    const key = `${left}${right}`;
    if (key === sent) return;
    sent = key;
    reportSwipeScrollEdges({ left, right }).catch(() => {
      dead = true;
    });
  };
  const schedule = () => {
    if (!frame) frame = requestAnimationFrame(measure);
  };
  const onPointer = (e: PointerEvent) => {
    x = e.clientX;
    y = e.clientY;
    schedule();
  };
  window.addEventListener("pointermove", onPointer, { passive: true });
  document.addEventListener("scroll", schedule, { capture: true, passive: true });
  window.addEventListener("resize", schedule, { passive: true });
  return () => {
    dead = true;
    if (frame) cancelAnimationFrame(frame);
    window.removeEventListener("pointermove", onPointer);
    document.removeEventListener("scroll", schedule, { capture: true });
    window.removeEventListener("resize", schedule);
  };
}

export function useSwipeNav(history: History) {
  const latest = useRef(history);
  latest.current = history;

  useEffect(() => {
    if (!isMacTauriShell()) return;
    const stopReporting = reportScrollRoom();
    let unlisten: (() => void) | undefined;
    let disposed = false;
    let settled = false;
    void listenToWindow<SwipeProgress>("native-swipe", (payload) => {
      const h = latest.current;
      const back = payload.amount > 0;
      if (payload.phase === "moving") {
        settled = false;
        if (!(back ? h.canBack : h.canForward)) return;
        const el = handle(back ? "back" : "forward");
        if (el) show(el, Math.abs(payload.amount), back ? 1 : -1);
        return;
      }
      if (settled) return;
      settled = true;
      for (const direction of ["back", "forward"] as const) {
        const el = handle(direction);
        if (el) hide(el);
      }
      if (payload.phase !== "ended" || Math.abs(payload.amount) < COMMIT) return;
      if (back && h.canBack) h.back();
      else if (!back && h.canForward) h.forward();
    })
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      stopReporting();
      unlisten?.();
    };
  }, []);
}
