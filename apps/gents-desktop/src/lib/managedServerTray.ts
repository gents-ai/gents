import type {
  ManagedServerAuthorityInput,
  ManagedServerStatus,
} from "@source-inc/gents-desktop-client";

import {
  managedServerWaitKind,
  unsettledManagedServerError,
  type ManagedServerWait,
} from "./managedServerStartup";

type Unlisten = () => void;
type Listen = (event: string, handler: () => void) => Promise<Unlisten>;

export const MANAGED_SERVER_TRAY_START_EVENT = "desktop://managed-server-tray-start";
export const MANAGED_SERVER_TRAY_STOP_EVENT = "desktop://managed-server-tray-stop";
export const MANAGED_SERVER_TRAY_RESTART_EVENT =
  "desktop://managed-server-tray-restart";

/** What the menu bar commands do to the local server, through its owner. */
export type TrayServer = {
  /** the status now, or a rejection saying why it could not be read */
  readStatus: () => Promise<ManagedServerStatus>;
  start: (agentName: string) => Promise<ManagedServerStatus>;
  stop: () => Promise<ManagedServerStatus>;
  restart: (
    agentName: string,
    authority: ManagedServerAuthorityInput,
  ) => Promise<ManagedServerStatus>;
  /** waits while the service boots, updates or awaits approval */
  settle: (status: ManagedServerStatus) => Promise<ManagedServerStatus>;
  offers: { start: boolean; stop: boolean; restart: boolean };
  /** calls back with each wait the owner publishes; returns the end of it */
  watchWait: (listener: (wait: ManagedServerWait | null) => void) => Unlisten;
};

export function installManagedServerTrayListeners(
  server: TrayServer,
  listen: Listen,
  reportError: (message: string) => void,
  showSetup: () => Promise<void> = async () => {},
  onWait: (wait: ManagedServerWait | null) => void = () => {},
): Unlisten {
  let cancelled = false;
  const cleanups: Unlisten[] = [];

  /* a command's progress is shown while it runs, not another screen's */
  const register = (event: string, action: () => Promise<unknown>) => {
    void listen(event, () => {
      const unwatch = server.watchWait(onWait);
      void action()
        .catch((cause) => {
          reportError(cause instanceof Error ? cause.message : String(cause));
        })
        .finally(() => {
          unwatch();
          onWait(null);
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
    const current = await server.readStatus();
    if (!current.effectiveToolCeiling) {
      await showSetup();
      throw new Error(
        "Complete local agent setup to review host access before starting the agent.",
      );
    }
    if (!server.offers.start)
      throw new Error("Start Agent is unavailable in this build.");
    await server.start(current.agentName?.trim() || "Local Agent");
  });
  register(MANAGED_SERVER_TRAY_STOP_EVENT, async () => {
    const current = await server.readStatus();
    if (current.state === "external") {
      throw new Error(
        "This agent was started outside the managed service. Stop that gents server process directly.",
      );
    }
    if (!server.offers.stop)
      throw new Error("Stop Agent is unavailable in this build.");
    await server.stop();
  });
  register(MANAGED_SERVER_TRAY_RESTART_EVENT, async () => {
    const current = await server.readStatus();
    if (current.state === "external") {
      throw new Error(
        "This agent was started outside the managed service. Stop that gents server process directly before restarting the managed agent.",
      );
    }
    // Restarting a runtime that is migrating its data would interrupt the
    // migration; the command waits for it to finish instead.
    if (managedServerWaitKind(current) === "updating") {
      const settled = await server.settle(current);
      const unsettled = unsettledManagedServerError(settled);
      if (unsettled) throw unsettled;
      return;
    }
    if (!current.effectiveToolCeiling) {
      throw new Error(
        "Restart is unavailable until the agent reports its confirmed host access. Open Gents and check the local agent status.",
      );
    }
    if (!server.offers.restart) {
      throw new Error("Restart Agent is unavailable in this build.");
    }
    await server.restart(current.agentName?.trim() || "Local Agent", {
      toolCeiling: current.effectiveToolCeiling,
      toolRoot: current.effectiveToolRoot,
    });
  });

  return () => {
    cancelled = true;
    cleanups.splice(0).forEach((cleanup) => cleanup());
  };
}
