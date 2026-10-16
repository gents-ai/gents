import { useEffect } from "react";
import { useStore } from "zustand";

import type { DesktopClientUpdatedListenerFactory } from "@source-inc/gents-desktop-client";

import type { DesktopApp } from "./desktopApp";
import { startClientObservation } from "./clientObservation";
import { clientStatus } from "./clientStore";

export function useClientRuntime(
  app: DesktopApp,
  listenToUpdates: DesktopClientUpdatedListenerFactory,
) {
  useStartup(app);
  useEffect(() => startClientObservation(app, listenToUpdates), [app, listenToUpdates]);
}

function useStartup({
  api,
  stores,
  lifecycle,
}: Pick<DesktopApp, "api" | "stores" | "lifecycle">) {
  useEffect(() => {
    void lifecycle.initializeDesktop();
  }, [lifecycle]);

  const needsHint = useStore(
    stores.client,
    (state) =>
      state.startupPhase === "managed-server-error" &&
      !state.startupDiagnosticsHint &&
      !state.snapshot,
  );
  useEffect(() => {
    if (!needsHint) return;
    let current = true;
    Promise.resolve()
      .then(() => api.fetchDesktopSnapshot())
      .then((next) => {
        if (current)
          clientStatus.setDiagnosticsHint(
            stores.client,
            next.bootstrap.diagnosticsHint || null,
          );
      })
      .catch(() => {});
    return () => {
      current = false;
    };
  }, [api, needsHint, stores]);
}
