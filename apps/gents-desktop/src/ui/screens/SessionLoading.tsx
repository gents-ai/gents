/* The session's screen before the session is here: one line of braille,
   centred, filling along a diagonal and draining again. Nothing else is
   drawn — no header, no composer — so the loaded screen arrives whole
   instead of assembling in front of the reader, title after header after
   chrome.

   A load that fails resolves to the screen with its LoadingStatus. A load
   that never answers resolves to nothing, since no read here has a
   timeout; after a while the line says so and offers the way back, so a
   stuck runtime does not leave a person looking at a pulse. */
import { ArrowLeft } from "lucide-react";
import { useEffect, useState } from "react";
import { href } from "@/lib/router";

/* four braille cells, filled corner to corner and emptied the same way:
   the "Ruminating" one-line loader from sacred.computer, at its cadence */
const FRAMES = [
  "⠁⠀⠀⠀",
  "⠋⠀⠀⠀",
  "⠟⠁⠀⠀",
  "⡿⠋⠀⠀",
  "⣿⠟⠁⠀",
  "⣿⡿⠋⠀",
  "⣿⣿⠟⠁",
  "⣿⣿⡿⠋",
  "⣿⣿⣿⠟",
  "⣿⣿⣿⡿",
  "⣿⣿⣿⣿",
  "⣿⣿⣿⣿",
  "⣾⣿⣿⣿",
  "⣴⣿⣿⣿",
  "⣠⣾⣿⣿",
  "⢀⣴⣿⣿",
  "⠀⣠⣾⣿",
  "⠀⢀⣴⣿",
  "⠀⠀⣠⣾",
  "⠀⠀⢀⣴",
  "⠀⠀⠀⣠",
  "⠀⠀⠀⢀",
  "⠀⠀⠀⠀",
  "⠀⠀⠀⠀",
];
const FRAME_MS = 60;
/* a session read is a local store read; past this it is not slow, it is stuck */
const STILL_WAITING_MS = 8000;

/* read when the line mounts, not when the module loads: a test host has no
   matchMedia until its setup runs */
const still = () => {
  try {
    return matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    return false;
  }
};

export function SessionLoading() {
  /* under reduced motion the line holds its full frame */
  const [STILL] = useState(still);
  const [frame, setFrame] = useState(() => (STILL ? FRAMES.indexOf("⣿⣿⣿⣿") : 0));
  const [waiting, setWaiting] = useState(false);
  useEffect(() => {
    if (STILL) return;
    const t = setInterval(() => setFrame((f) => (f + 1) % FRAMES.length), FRAME_MS);
    return () => clearInterval(t);
  }, []);
  useEffect(() => {
    const t = setTimeout(() => setWaiting(true), STILL_WAITING_MS);
    return () => clearTimeout(t);
  }, []);
  return (
    <div
      role="status"
      aria-label="Loading session"
      data-testid="session-loading"
      data-waiting={waiting || undefined}
      className="grid h-full place-items-center"
    >
      <div className="flex flex-col items-center gap-4">
        <span
          className="font-mono text-base leading-none text-foreground select-none"
          aria-hidden="true"
        >
          {FRAMES[frame]}
        </span>
        <div
          className={`flex flex-col items-center gap-1 text-center text-sm text-muted-foreground transition-opacity duration-300 motion-reduce:transition-none ${waiting ? "opacity-100" : "pointer-events-none opacity-0"}`}
          aria-hidden={!waiting}
        >
          <p>Still loading this session.</p>
          <a
            href={href({ name: "sessions" })}
            className="inline-flex items-center gap-1 hover:text-foreground"
            tabIndex={waiting ? 0 : -1}
          >
            <ArrowLeft className="size-3.5" /> Sessions
          </a>
        </div>
      </div>
    </div>
  );
}
