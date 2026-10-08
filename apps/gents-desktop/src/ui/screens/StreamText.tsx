/* Markdown that is still arriving, revealed at a steady pace. The pacing
   and the reasons for it are in stream-reveal.ts. */
import { useEffect, useMemo, useRef, useState } from "react";
import { Markdown } from "./Markdown";
import {
  closeOpenFence,
  initialReveal,
  revealedText,
  stepReveal,
  type RevealState,
} from "./stream-reveal";

/* a person who asked for less motion sees text as it lands */
function smoothingOn() {
  try {
    return !matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    return true;
  }
}

export function StreamText({ text }: { text: string }) {
  const [enabled] = useState(smoothingOn);
  /* The pace advances every frame, in a ref; React hears about it only when
     a frame reaches the next word boundary and the drawn text changes. A
     state update per frame kept React committing about sixty times a
     second while a reply streamed, which every keystroke had to wait on. */
  const [initial] = useState(() =>
    initialReveal(enabled ? 0 : text.length, text.length),
  );
  const reveal = useRef<RevealState>(initial);
  const [visible, setVisible] = useState(() =>
    enabled ? revealedText(text, initial.shown).length : text.length,
  );
  const drawn = useRef(visible);
  const target = useRef(text);
  target.current = text;
  const running = enabled && visible < text.length;

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
      const next = stepReveal(reveal.current, target.current.length, now, dt);
      reveal.current = next;
      const length = revealedText(target.current, next.shown).length;
      if (length !== drawn.current) {
        drawn.current = length;
        setVisible(length);
      }
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [running]);

  const shown = !enabled || visible >= text.length ? text : text.slice(0, visible);
  const display = shown.length < text.length ? closeOpenFence(shown) : shown;
  /* a frame that did not reach the next word boundary changes nothing */
  /* returned bare, for the reason in ReplyText: a wrapper here
     takes the spacing off every block inside it */
  return useMemo(() => (display ? <Markdown>{display}</Markdown> : null), [display]);
}
