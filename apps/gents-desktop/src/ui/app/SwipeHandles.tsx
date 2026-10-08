/* Chrome's history-swipe affordance: a chip slides in from the pane's edge
   as a two-finger swipe progresses and fills solid once the gesture will
   commit. Driven by direct style writes from the swipe hook, since progress
   arrives at display rate. */
import { ArrowLeft, ArrowRight } from "lucide-react";

export const SWIPE_HANDLE_ATTR = "data-swipe-handle";

const chip =
  "absolute top-1/2 flex size-10 -translate-y-1/2 items-center justify-center overflow-hidden rounded-full border border-border bg-popover text-foreground shadow-md opacity-0 data-[armed]:border-ink data-[armed]:text-ink-foreground";

function Chip({ direction }: { direction: "back" | "forward" }) {
  const Arrow = direction === "back" ? ArrowLeft : ArrowRight;
  const side =
    direction === "back" ? "left-0 -translate-x-full" : "right-0 translate-x-full";
  return (
    <div
      {...{ [SWIPE_HANDLE_ATTR]: direction }}
      className={`${chip} ${side}`}
      aria-hidden
    >
      <span data-swipe-fill className="absolute inset-0 bg-ink opacity-0" />
      <Arrow className="relative size-4" />
    </div>
  );
}

/* The clip keeps the resting chips, which sit just past the pane's edges,
   out of the pane's scrollable overflow; otherwise a focus call can scroll
   the pane sideways to reveal them. */
export function SwipeHandles() {
  return (
    <div
      className="pointer-events-none absolute inset-0 z-40 overflow-clip"
      aria-hidden
    >
      <Chip direction="back" />
      <Chip direction="forward" />
    </div>
  );
}
