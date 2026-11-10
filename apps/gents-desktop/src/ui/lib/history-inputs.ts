// The keys and mouse buttons a person already uses for history in a
// browser, on the app's own history; a field being typed in keeps its keys,
// and so does a control that moves with arrow keys (Alt+← on a tab or a
// divider moves it, and must not also go back).
import { useEffect, useRef } from "react";
import { keyTaken } from "@/screens/shortcutKeys";
import type { History } from "./router";

const typing = (target: EventTarget | null) =>
  target instanceof HTMLElement &&
  Boolean(target.closest('input, textarea, select, [contenteditable="true"]'));

export function useHistoryInputs(history: History) {
  const latest = useRef(history);
  latest.current = history;

  useEffect(() => {
    const mac = navigator.platform.toUpperCase().includes("MAC");
    const onKey = (e: KeyboardEvent) => {
      const history = latest.current;
      if (typing(e.target) || keyTaken(e)) return;
      const chord = mac ? e.metaKey && !e.altKey : e.altKey && !e.metaKey;
      if (!chord || e.ctrlKey || e.shiftKey) return;
      const back = mac ? e.key === "[" : e.key === "ArrowLeft";
      const forward = mac ? e.key === "]" : e.key === "ArrowRight";
      if (back && history.canBack) {
        e.preventDefault();
        history.back();
      } else if (forward && history.canForward) {
        e.preventDefault();
        history.forward();
      }
    };
    const onMouse = (e: MouseEvent) => {
      const history = latest.current;
      if (e.button === 3 && history.canBack) {
        e.preventDefault();
        history.back();
      } else if (e.button === 4 && history.canForward) {
        e.preventDefault();
        history.forward();
      }
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("mouseup", onMouse);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("mouseup", onMouse);
    };
  }, []);
}
