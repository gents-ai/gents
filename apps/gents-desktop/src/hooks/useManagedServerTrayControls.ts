import { useEffect } from "react";
import { toast } from "sonner";

import { describeManagedServerWait } from "../lib/managedServerStartup";
import {
  installManagedServerTrayListeners,
  type TrayServer,
} from "../lib/managedServerTray";
import type { DesktopApp } from "./desktopApp";
import { inNativeShell, listenToWindow, showAndFocusWindow } from "../lib/nativeShell";
import {
  ownsAutomaticRecovery,
  supportsLocalManagedServer,
} from "../lib/shellPlatform";

const TRAY_WAIT_TOAST = "managed-server-tray-wait";

/** The native tray targets the one view that owns shared-backend recovery. */
export function useManagedServerTrayControls({ actions, stores }: DesktopApp) {
  useEffect(() => {
    if (
      !ownsAutomaticRecovery() ||
      !supportsLocalManagedServer() ||
      !actions.localServerOffers.status ||
      !inNativeShell()
    )
      return;

    const server = trayServerFor({ actions, stores });
    return installManagedServerTrayListeners(
      server,
      (event, handler) => listenToWindow(event, handler),
      (message) => {
        void (async () => {
          try {
            await showAndFocusWindow();
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
      showAndFocusWindow,
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
  }, [actions, stores]);
}

/** The local server as the menu bar commands use it, through its owner. */
export function trayServerFor({
  actions,
  stores,
}: Pick<DesktopApp, "actions" | "stores">): TrayServer {
  return {
    readStatus: async () => {
      const status = await actions.refreshLocalServer();
      if (status) return status;
      throw new Error(
        `Could not check the background agent: ${stores.localServer.getState().readFailure}`,
      );
    },
    start: (agentName) => actions.startLocalServer(agentName),
    stop: () => actions.stopLocalServer(),
    restart: (agentName, authority) => actions.restartLocalServer(agentName, authority),
    settle: (status) => actions.settleLocalServer(status),
    offers: actions.localServerOffers,
    watchWait: (listener) =>
      stores.localServer.subscribe((state, prev) => {
        if (state.wait !== prev.wait) listener(state.wait);
      }),
  };
}
