/* Markdown that is still arriving, revealed at a steady pace. The pacing
   and the reasons for it are in stream-reveal.ts. */
import { createContext, useEffect, useMemo, useRef, useState } from "react";
import { Markdown } from "./Markdown";
import {
  closeOpenFence,
  initialReveal,
  revealedText,
  stepReveal,
  type Handoff,
} from "./stream-reveal";

/* a person who asked for less motion sees text as it lands */
function smoothingOn() {
  try {
    return !matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    return true;
  }
}

/* What the transcript knows about streaming: where the live text hands
   over, and which items were already there when it opened — history is
   never typed out again. */
export type StreamScope = { handoff: Handoff; isNew: (itemKey: string) => boolean };
export const StreamContext = createContext<StreamScope | null>(null);

export function StreamText({
  text,
  startFrom = 0,
  onShown,
}: {
  text: string;
  /* how much is already on screen, carried over from the live tail */
  startFrom?: number;
  onShown?: (shown: string) => void;
}) {
  const [enabled] = useState(smoothingOn);
  const [state, setState] = useState(() =>
    initialReveal(enabled ? startFrom : text.length, text.length),
  );
  const target = useRef(text);
  target.current = text;
  const running = enabled && state.shown < text.length;

  useEffect(() => {
    if (!running) return;
    let frame = 0;
    let last = performance.now();
    const tick = (now: number) => {
      const dt = now - last;
      last = now;
      /* every frame's result is kept, fractions included: at the slowest
         pace a frame is worth less than one character, and a frame that
         dropped its fraction would never reach the next one */
      setState((s) => stepReveal(s, target.current.length, now, dt));
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [running]);

  const shown = enabled ? revealedText(text, state.shown) : text;
  const display = shown.length < text.length ? closeOpenFence(shown) : shown;
  useEffect(() => {
    onShown?.(shown);
  }, [shown, onShown]);
  /* a frame that did not reach the next word boundary changes nothing */
  /* returned bare, for the reason in AssistantContent: a wrapper here
     takes the spacing off every block inside it */
  return useMemo(() => (display ? <Markdown>{display}</Markdown> : null), [display]);
}
