import { useEffect } from "react";
import { announceWindowSetupComplete, windowLabel } from "../lib/nativeShell";
import { isMacTauriShell } from "../lib/shellPlatform";

export function useNativeWindowReadiness(setupComplete: boolean) {
  useEffect(() => {
    if (!setupComplete || !isMacTauriShell() || windowLabel() !== "main") return;
    void announceWindowSetupComplete();
  }, [setupComplete]);
}
