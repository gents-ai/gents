import { useEffect } from "react";
import { announceWindowSetupComplete, windowLabel } from "../lib/nativeShell";
import { isMacTauriShell } from "../lib/shellPlatform";

/** Publish the existing onboarding owner's decision; the host latches it. */
export function useNativeWindowReadiness(setupComplete: boolean) {
  useEffect(() => {
    if (!setupComplete || !isMacTauriShell() || windowLabel() !== "main") return;
    void announceWindowSetupComplete();
  }, [setupComplete]);
}
