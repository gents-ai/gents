import { useEffect } from "react";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";

import type { ClientLifecycle } from "./clientLifecycle";
import type { ClientStore } from "./clientStore";
import { useIncompatibleHome } from "./useIncompatibleHome";

export type { DesktopStartupPhase } from "../lib/loadingStatus";

/** Starts the desktop once mounted, and reads startup's state for screens. */
export function useDesktopClientLifecycle(
  api: DesktopApiAdapter,
  client: ClientStore,
  lifecycle: ClientLifecycle,
) {
  const {
    snapshot,
    error,
    startupPhase,
    starting,
    stopping,
    managedServerWait,
    managedServerFailure,
    startupDiagnosticsHint,
  } = useStore(
    client,
    useShallow((state) => ({
      snapshot: state.snapshot,
      error: state.error,
      startupPhase: state.startupPhase,
      starting: state.starting,
      stopping: state.stopping,
      managedServerWait: state.managedServerWait,
      managedServerFailure: state.managedServerFailure,
      startupDiagnosticsHint: state.startupDiagnosticsHint,
    })),
  );
  const managedServerFailed = startupPhase === "managed-server-error";

  // A managed-server failure precedes the first snapshot read; later startup
  // errors already have one. The bootstrap summary of a client-less snapshot
  // names where the logs are, and is read here without publishing it.
  useEffect(() => {
    if (!managedServerFailed || startupDiagnosticsHint || snapshot) return;
    let current = true;
    Promise.resolve()
      .then(() => api.fetchDesktopSnapshot())
      .then((next) => {
        if (current)
          client.setState({
            startupDiagnosticsHint: next.bootstrap.diagnosticsHint || null,
          });
      })
      .catch(() => {});
    return () => {
      current = false;
    };
  }, [api, managedServerFailed, startupDiagnosticsHint, snapshot]);

  useEffect(() => {
    void lifecycle.initializeDesktop();
  }, [lifecycle]);

  return {
    snapshot,
    error,
    startupPhase,
    starting,
    stopping,
    incompatibleHome: useIncompatibleHome(client, lifecycle.home),
    managedServerWait,
    diagnosticsHint: snapshot?.bootstrap.diagnosticsHint || startupDiagnosticsHint,
    canRestartManagedServer: Boolean(
      managedServerFailure?.status.agentName &&
      managedServerFailure.status.effectiveToolCeiling &&
      api.restartManagedServer,
    ),
  };
}
