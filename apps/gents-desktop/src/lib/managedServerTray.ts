import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";

type Unlisten = () => void;
type Listen = (event: string, handler: () => void) => Promise<Unlisten>;

export const MANAGED_SERVER_TRAY_START_EVENT = "desktop://managed-server-tray-start";
export const MANAGED_SERVER_TRAY_STOP_EVENT = "desktop://managed-server-tray-stop";
export const MANAGED_SERVER_TRAY_RESTART_EVENT =
  "desktop://managed-server-tray-restart";

export function installManagedServerTrayListeners(
  api: DesktopApiAdapter,
  listen: Listen,
  reportError: (message: string) => void,
  showSetup: () => Promise<void> = async () => {},
): Unlisten {
  let cancelled = false;
  const cleanups: Unlisten[] = [];

  const register = (event: string, action: () => Promise<unknown>) => {
    void listen(event, () => {
      void action().catch((cause) => {
        reportError(cause instanceof Error ? cause.message : String(cause));
      });
    })
      .then((cleanup) => {
        if (cancelled) cleanup();
        else cleanups.push(cleanup);
      })
      .catch((cause) => {
        reportError(
          `Could not register the agent menu command: ${cause instanceof Error ? cause.message : String(cause)}`,
        );
      });
  };

  register(MANAGED_SERVER_TRAY_START_EVENT, async () => {
    const current = await api.managedServerStatus!();
    if (!current.effectiveToolCeiling) {
      await showSetup();
      throw new Error(
        "Complete local agent setup to review host access before starting the agent.",
      );
    }
    if (!api.startManagedServer)
      throw new Error("Start Agent is unavailable in this build.");
    await api.startManagedServer(current.agentName?.trim() || "Local Agent");
  });
  register(MANAGED_SERVER_TRAY_STOP_EVENT, async () => {
    const current = await api.managedServerStatus!();
    if (current.state === "external") {
      throw new Error(
        "This agent was started outside the managed service. Stop that gents server process directly.",
      );
    }
    if (!api.stopManagedServer)
      throw new Error("Stop Agent is unavailable in this build.");
    await api.stopManagedServer(false);
  });
  register(MANAGED_SERVER_TRAY_RESTART_EVENT, async () => {
    const current = await api.managedServerStatus!();
    if (current.state === "external") {
      throw new Error(
        "This agent was started outside the managed service. Stop that gents server process directly before restarting the managed agent.",
      );
    }
    if (!current.effectiveToolCeiling) {
      throw new Error(
        "Restart is unavailable until the agent reports its confirmed host access. Open Gents and check the local agent status.",
      );
    }
    if (!api.restartManagedServer) {
      throw new Error("Restart Agent is unavailable in this build.");
    }
    await api.restartManagedServer(current.agentName?.trim() || "Local Agent", {
      toolCeiling: current.effectiveToolCeiling,
      toolRoot: current.effectiveToolRoot,
    });
  });

  return () => {
    cancelled = true;
    cleanups.splice(0).forEach((cleanup) => cleanup());
  };
}
