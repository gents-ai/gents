import { useState } from "react";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import { needsFirstRunSetup } from "../lib/firstRun";
import { useApp } from "./AppContext";

type Phase = "unknown" | "active" | "done";

/**
 * Whether first-run setup owns the window. Decided once startup is ready
 * and its snapshot says a home needs it, then held: the wizard owns its
 * own starting page and must not remount at welcome when the server it
 * started comes up. A completed home reset (a new `generation`) starts
 * over from detection.
 */
export function useFirstRun(generation: number, startupReady: boolean) {
  /* what of the client decides it, as two facts, so a read that changes
     neither re-renders nothing */
  const { read, needsSetup } = useStore(
    useApp().stores.client,
    useShallow((state) => ({
      read: state.snapshot !== null,
      needsSetup: state.snapshot !== null && needsFirstRunSetup(state.snapshot),
    })),
  );
  const [held, setHeld] = useState<{ generation: number; phase: Phase }>({
    generation,
    phase: "unknown",
  });
  let phase: Phase = held.generation === generation ? held.phase : "unknown";
  /* decided either way: a home that needed no setup at startup does not get
     the wizard later because its agent loses its inference */
  if (phase === "unknown" && startupReady && read) {
    phase = needsSetup ? "active" : "done";
    setHeld({ generation, phase });
  }
  return {
    phase,
    /** the snapshot has been read and setup is not needed, or is done */
    settled: phase === "done" || (phase === "unknown" && read),
    finish: () => setHeld({ generation, phase: "done" }),
  };
}
