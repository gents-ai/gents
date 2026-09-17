import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isMacTauriShell } from "../lib/shellPlatform";

/** Publish the existing onboarding owner's decision; the host latches it. */
export function useNativeWindowReadiness(setupComplete: boolean) {
  useEffect(() => {
    if (!setupComplete || !isMacTauriShell() || getCurrentWindow().label !== "main")
      return;
    void invoke("desktop_window_setup_complete");
  }, [setupComplete]);
}
