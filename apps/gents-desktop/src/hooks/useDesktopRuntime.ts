import { useEffect } from "react";
import { useStore } from "zustand";

import {
  selectedBehaviorIdForDeployment,
  type DesktopClientUpdatedListenerFactory,
} from "@source-inc/gents-desktop-client";

import { isMacTauriShell } from "../lib/shellPlatform";
import type { DesktopApp } from "./desktopApp";
import { selection } from "./selectionStore";
import { useDesktopProjectionEffects } from "./useDesktopProjectionEffects";
import { clientRunning, clientStatus } from "./clientStore";
import { firstNode, nodeOf } from "./fleetStore";

/**
 * What the app does on its own while it runs: starts and recovers the
 * client, observes the bridge, keeps the selection valid against what
 * the nodes list, and tells the host which node is selected. Mounted
 * once, at the root; each part selects only what it reacts to.
 */
export function useDesktopRuntime(
  app: DesktopApp,
  listenToUpdates: DesktopClientUpdatedListenerFactory,
) {
  useStartup(app);
  useDesktopProjectionEffects(app, listenToUpdates);
  useSelectionReconcile(app);
  usePublishedSelection(app);
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

/** The selection kept valid against what the nodes list. */
export function useSelectionReconcile({
  stores,
  actions,
}: {
  stores: DesktopApp["stores"];
  actions: Pick<DesktopApp["actions"], "selectAgent">;
}) {
  const store = stores.selection;
  const agentDid = useStore(store, (state) => state.agentDid);
  const behaviorId = useStore(store, (state) => state.behaviorId);
  const composingFor = useStore(store, (state) => state.composingFor);
  const firstNodeDid = useStore(
    stores.fleet,
    (state) => firstNode(state)?.agentDid ?? null,
  );
  const node = useStore(stores.fleet, (state) => nodeOf(state, agentDid));

  // Snapshot absence is not an explicit navigation intent. Preserve an
  // existing principal selection while bounded observations catch up; only
  // initialize an empty selection through the route owner. The route's
  // effect runs first in the same commit (it is a child's), so the store,
  // not this render's value, says whether a node is already selected.
  useEffect(() => {
    if (!store.getState().agentDid && firstNodeDid) actions.selectAgent(firstNodeDid);
  }, [actions, agentDid, firstNodeDid, store]);

  // Read from the stores for the same reason: the route may have selected a
  // node and behavior in this commit. The subscriptions above only rerun it.
  useEffect(() => {
    const current = store.getState();
    const selected = current.agentDid
      ? (stores.fleet.getState().nodes[current.agentDid] ?? null)
      : null;
    if (!selected) {
      selection.settleBehavior(store, null);
      return;
    }
    // A mailbox tap or the new-session screen chose this behavior. Preserve
    // it while the independently replicated behavior and session rows catch
    // up; explicit navigation lets go of it in the selection store.
    if (current.composingFor === selected.agentDid) return;
    // A snapshot may reconcile behavior availability, never user session
    // selection. Null is an intentional fresh composer, not a request to open
    // the first matching session. A missing selected row stays selected while
    // hydration/error presentation handles its availability (ClientShell's
    // snapshot_preserves_selection contract).
    selection.settleBehavior(
      store,
      selectedBehaviorIdForDeployment(selected, current.behaviorId),
    );
  }, [behaviorId, composingFor, node, store, stores.fleet]);
}

/** The host narrows its observation to the selected node. */
function usePublishedSelection({
  api,
  stores,
  lifecycle,
}: Pick<DesktopApp, "api" | "stores" | "lifecycle">) {
  const clientAvailable = useStore(stores.client, clientRunning);
  const agentDid = useStore(stores.selection, (state) => state.agentDid);
  useEffect(() => {
    if (!clientAvailable) return;
    let disposed = false;
    const publishSelection = () => {
      void api.setSelectedAgent(agentDid).catch((err) => {
        if (!disposed) lifecycle.setError(String(err));
      });
    };
    publishSelection();
    // Closing siblings returns focus to the surviving view. Republish so the
    // host can narrow observation again after returning to a single window.
    if (isMacTauriShell()) window.addEventListener("focus", publishSelection);
    return () => {
      disposed = true;
      window.removeEventListener("focus", publishSelection);
    };
  }, [api, agentDid, clientAvailable, lifecycle]);
}
