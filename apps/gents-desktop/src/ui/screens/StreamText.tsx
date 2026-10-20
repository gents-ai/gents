import { useEffect, useMemo, useRef, useState } from "react";
import { Markdown } from "./Markdown";
import {
  closeOpenFence,
  initialReveal,
  revealedText,
  stepReveal,
  type RevealState,
} from "./stream-reveal";

function smoothingOn() {
  try {
    return !matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    return true;
  }
}

export function StreamText({
  text,
  directives = false,
}: {
  text: string;
  /** as Markdown's: the caller says whether this is the model's own reply */
  directives?: boolean;
}) {
  const [enabled] = useState(smoothingOn);
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
  return useMemo(
    () => (display ? <Markdown directives={directives}>{display}</Markdown> : null),
    [display, directives],
  );
}
