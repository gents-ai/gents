import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import { toast } from "sonner";

import { describeManagedServerWait } from "../lib/managedServerStartup";
import { installManagedServerTrayListeners } from "../lib/managedServerTray";
import {
  ownsAutomaticRecovery,
  supportsLocalManagedServer,
} from "../lib/shellPlatform";

const TRAY_WAIT_TOAST = "managed-server-tray-wait";

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
      (wait) => {
        try {
          if (!wait) toast.dismiss(TRAY_WAIT_TOAST);
          else
            toast.loading(describeManagedServerWait(wait, Date.now()).label, {
              id: TRAY_WAIT_TOAST,
            });
        } catch {
          // Progress is informational; the command's own result reports failures.
        }
      },
    );
  }, [api]);
}
