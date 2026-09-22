import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { toast } from "sonner";

import { installManagedServerTrayListeners } from "../lib/managedServerTray";
import {
  ownsAutomaticRecovery,
  supportsLocalManagedServer,
} from "../lib/shellPlatform";

/** The native tray targets the one view that owns shared-backend recovery. */
export function useManagedServerTrayControls(api: DesktopApiAdapter) {
  useEffect(() => {
    if (
      !ownsAutomaticRecovery() ||
      !supportsLocalManagedServer() ||
      !api.managedServerStatus ||
      !("__TAURI_INTERNALS__" in window)
    )
      return;

    const view = getCurrentWindow();
    return installManagedServerTrayListeners(
      api,
      (event, handler) => view.listen(event, handler),
      (message) => {
        void (async () => {
          try {
            await view.show();
            await view.setFocus();
          } catch {
            // The command error remains actionable even if the OS cannot reveal
            // the owner window; never replace it with a secondary focus error.
          }
          try {
            toast.error(`Agent menu command failed: ${message}`);
          } catch {
            // A notification renderer failure must not become an unhandled task.
          }
        })();
      },
      async () => {
        await view.show();
        await view.setFocus();
      },
    );
  }, [api]);
}
