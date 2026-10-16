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
          }
          try {
            toast.error(`Node menu command failed: ${message}`);
          } catch {
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
        }
      },
    );
  }, [actions, stores]);
}

export function trayServerFor({
  actions,
  stores,
}: Pick<DesktopApp, "actions" | "stores">): TrayServer {
  return {
    readStatus: async () => {
      const status = await actions.refreshLocalServer();
      if (status) return status;
      throw new Error(
        `Could not check the background node: ${stores.localServer.getState().readFailure}`,
      );
    },
    start: (nodeName) => actions.startLocalServer(nodeName),
    stop: () => actions.stopLocalServer(),
    restart: (nodeName, authority) => actions.restartLocalServer(nodeName, authority),
    settle: (status) => actions.settleLocalServer(status),
    offers: actions.localServerOffers,
    watchWait: (listener) =>
      stores.localServer.subscribe((state, prev) => {
        if (state.wait !== prev.wait) listener(state.wait);
      }),
  };
}
