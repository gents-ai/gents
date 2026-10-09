import { useEffect } from "react";
import { useStore } from "zustand";

import type { DesktopClientUpdatedListenerFactory } from "@source-inc/gents-desktop-client";

import type { DesktopApp } from "./desktopApp";
import { startClientObservation } from "./clientObservation";
import { clientStatus } from "./clientStore";

/**
 * What the app does on its own while it is mounted: starts the client and
 * follows it while it runs. Mounted once, at the root. Recovery, selection
 * reconciliation and the transcript's workflow are reactions the app sets
 * up when it is made.
 */
export function useClientRuntime(
  app: DesktopApp,
  listenToUpdates: DesktopClientUpdatedListenerFactory,
) {
  useStartup(app);
  useEffect(() => startClientObservation(app, listenToUpdates), [app, listenToUpdates]);
}

/** Startup on mount, and where the logs are when the managed server failed
    before any snapshot was read. */
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
  // The bootstrap summary of a client-less snapshot names where the logs
  // are, and is read here without publishing it.
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
