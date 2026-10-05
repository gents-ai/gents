import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";

import type { ClientLifecycle } from "./clientLifecycle";
import type { ClientStore } from "./clientStore";
import { useIncompatibleHome } from "./useIncompatibleHome";

export type { DesktopStartupPhase } from "../lib/loadingStatus";

/** Startup's state, as screens read it. */
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
