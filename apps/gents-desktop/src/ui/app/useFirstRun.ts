import { useState } from "react";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import {
  configIsRead,
  hasSavedRemote,
  needsFirstRunSetup,
  projectFirstRunPhase,
  type FirstRunPhase,
} from "../lib/firstRun";
import { useApp } from "./AppContext";

/**
 * Setup starts only when init.json is explicitly absent or a configured
 * home has a complete operator config read. Unreadable and partial state
 * remain pending. Once the wizard owns the window, it remains there until
 * its own completion callback releases it.
 */
export function useFirstRun(generation: number, startupReady: boolean) {
  const { stores } = useApp();
  const facts = useStore(
    stores.client,
    useShallow((state) => {
      const snapshot = state.snapshot;
      const initConfigStatus = snapshot?.bootstrap.initConfigStatus;
      const initNodeDid = snapshot?.bootstrap.initNodeDid;
      return {
        hasSnapshot: snapshot !== null,
        initConfigStatus,
        savedRemote: snapshot !== null && hasSavedRemote(snapshot),
        needsSetup: snapshot !== null && needsFirstRunSetup(snapshot),
        authoritative:
          initConfigStatus === "configured" &&
          snapshot !== null &&
          configIsRead(snapshot, initNodeDid),
      };
    }),
  );
  const [held, setHeld] = useState<{ generation: number; phase: FirstRunPhase }>({
    generation,
    phase: "unknown",
  });
  let phase: FirstRunPhase = held.generation === generation ? held.phase : "unknown";
  if (phase === "unknown") {
    phase = projectFirstRunPhase(
      phase,
      startupReady && facts.hasSnapshot,
      facts.initConfigStatus,
      facts.needsSetup,
      facts.authoritative,
      facts.savedRemote,
    );
  }
  if (held.generation !== generation || held.phase !== phase) {
    setHeld({ generation, phase });
  }
  return {
    phase,
    initConfigStatus: facts.initConfigStatus,
    settled: phase !== "unknown",
    finish: () => setHeld({ generation, phase: "done" }),
  };
}
