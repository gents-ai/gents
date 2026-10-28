import { useState } from "react";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import {
  configIsRead,
  needsFirstRunSetup,
  projectFirstRunPhase,
  type FirstRunPhase,
} from "../lib/firstRun";
import { useApp } from "./AppContext";
import { supportsLocalManagedServer } from "../../lib/shellPlatform";

/** Remote clients have no local init.json; persisted enrollment records setup
 * completion independently of live route readiness after a restart. */
export function useFirstRun(generation: number, startupReady: boolean) {
  const { stores } = useApp();
  const localRuntimeAllowed = supportsLocalManagedServer();
  const facts = useStore(
    stores.client,
    useShallow((state) => {
      const snapshot = state.snapshot;
      const initConfigStatus = snapshot?.bootstrap.initConfigStatus;
      const initNodeDid = snapshot?.bootstrap.initNodeDid;
      return {
        hasSnapshot: snapshot !== null,
        initConfigStatus,
        needsSetup: snapshot !== null && needsFirstRunSetup(snapshot),
        remoteConfigured:
          snapshot?.client?.deployments.some((node) => node.source === "enrollment") ??
          false,
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
      localRuntimeAllowed,
      facts.remoteConfigured,
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
