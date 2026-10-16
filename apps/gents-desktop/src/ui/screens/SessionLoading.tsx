import { ArrowLeft } from "lucide-react";
import { useEffect, useState } from "react";
import { href } from "@/lib/router";

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
const STILL_WAITING_MS = 8000;

const still = () => {
  try {
    return matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    return false;
  }
};

export function SessionLoading() {
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
