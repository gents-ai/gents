import { useState } from "react";

import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";

import { needsFirstRunSetup } from "../lib/firstRun";

type Phase = "unknown" | "active" | "done";

/**
 * Whether first-run setup owns the window. Decided once startup is ready
 * and its snapshot says a home needs it, then held: the wizard owns its
 * own starting page and must not remount at welcome when the server it
 * started comes up. A completed home reset (a new `generation`) starts
 * over from detection.
 */
export function useFirstRun(
  snapshot: DesktopClientSnapshot | null,
  generation: number,
  startupReady: boolean,
) {
  const [held, setHeld] = useState<{ generation: number; phase: Phase }>({
    generation,
    phase: "unknown",
  });
  let phase: Phase = held.generation === generation ? held.phase : "unknown";
  if (
    phase === "unknown" &&
    startupReady &&
    snapshot !== null &&
    needsFirstRunSetup(snapshot)
  ) {
    phase = "active";
    setHeld({ generation, phase });
  }
  return {
    phase,
    /** the snapshot has been read and setup is not needed, or is done */
    settled: phase === "done" || (phase === "unknown" && snapshot !== null),
    finish: () => setHeld({ generation, phase: "done" }),
  };
}
