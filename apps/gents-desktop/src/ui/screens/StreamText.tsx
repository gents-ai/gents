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

/* how much of each streamed reply has been shown, so a reply drawn again
   (its session reopened, its row scrolled back into view) starts where the
   reader left it instead of typing out from its first character */
const SHOWN_KEPT = 64;
const shownBefore = new Map<string, number>();
function keepShown(key: string, length: number) {
  shownBefore.delete(key);
  shownBefore.set(key, length);
  if (shownBefore.size > SHOWN_KEPT)
    shownBefore.delete(shownBefore.keys().next().value!);
}

export function StreamText({
  text,
  directives = false,
  onRevealed,
  revealKey,
}: {
  text: string;
  /** as Markdown's: the caller says whether this is the model's own reply */
  directives?: boolean;
  /** told the length of `text` once every character of it is shown, and
      null while some are not */
  onRevealed?: (length: number | null) => void;
  /** names the reply, so a fresh mount resumes from what was shown */
  revealKey?: string;
}) {
  const [enabled] = useState(smoothingOn);
  const [initial] = useState(() =>
    initialReveal(
      enabled
        ? Math.min(text.length, (revealKey && shownBefore.get(revealKey)) || 0)
        : text.length,
      text.length,
    ),
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
    if (revealKey) keepShown(revealKey, visible);
  }, [revealKey, visible]);
  const report = useRef(onRevealed);
  report.current = onRevealed;
  useEffect(
    () => report.current?.(running ? null : text.length),
    [running, text.length],
  );

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
