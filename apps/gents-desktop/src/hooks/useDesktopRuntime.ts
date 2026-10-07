import { useEffect, useState } from "react";
import { useStore } from "zustand";

import { reconcileProjectedWorkflow } from "@source-inc/gents-desktop-chat";
import {
  selectedBehaviorIdForDeployment,
  type DesktopClientUpdatedListenerFactory,
} from "@source-inc/gents-desktop-client";

import { isMacTauriShell, ownsAutomaticRecovery } from "../lib/shellPlatform";
import { setterOf } from "./chatStore";
import type { DesktopApp } from "./desktopApp";
import {
  logShellEvent,
  shouldAutoRestartP2P,
  timingConfig,
} from "./desktopShellRuntime";
import { selection } from "./selectionStore";
import { useDesktopProjectionEffects } from "./useDesktopProjectionEffects";

/**
 * What the app does on its own while it runs: starts and recovers the
 * client, observes the bridge, keeps the selection valid against what
 * the nodes list, tells the host which node is selected, and lets the
 * local workflow follow the transcript. Mounted once, at the root; each
 * part selects only what it reacts to.
 */
export function useDesktopRuntime(
  app: DesktopApp,
  listenToUpdates: DesktopClientUpdatedListenerFactory,
) {
  useStartup(app);
  useClientRecovery(app);
  useDesktopProjectionEffects(app, listenToUpdates);
  useSelectionReconcile(app);
  usePublishedSelection(app);
  useWorkflowReconcile(app);
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
          stores.client.setState({
            startupDiagnosticsHint: next.bootstrap.diagnosticsHint || null,
          });
      })
      .catch(() => {});
    return () => {
      current = false;
    };
  }, [api, needsHint, stores]);
}

/** Restarts a client whose P2P transport is wedged, at most once per
    cooldown. */
function useClientRecovery({
  stores,
  lifecycle,
}: Pick<DesktopApp, "stores" | "lifecycle">) {
  const { recovery } = lifecycle;
  const snapshot = useStore(stores.client, (state) => state.snapshot);
  const starting = useStore(stores.client, (state) => state.starting);
  const stopping = useStore(stores.client, (state) => state.stopping);
  const sending = useStore(stores.chat, (state) => state.sending);
  const runtimeHealth = snapshot?.client?.p2pHealth ?? null;

  useEffect(() => {
    const previousHealth = recovery.lastObservedP2PHealth;
    recovery.lastObservedP2PHealth = runtimeHealth;
    if (!runtimeHealth) return;
    if (runtimeHealth.status === "healthy") {
      recovery.lastP2PAutoRestartAt = null;
      return;
    }
    if (
      !ownsAutomaticRecovery() ||
      recovery.autoRestartInFlight ||
      starting ||
      stopping ||
      sending ||
      !shouldAutoRestartP2P(
        previousHealth,
        runtimeHealth,
        recovery.lastP2PAutoRestartAt,
        Date.now(),
        timingConfig().p2pAutoRestartCooldownMs,
      )
    )
      return;
    recovery.lastP2PAutoRestartAt = Date.now();
    logShellEvent(
      `auto restart requested reason="P2P transport wedged" status=${runtimeHealth.status} failures=${runtimeHealth.consecutiveFailures}`,
    );
    void lifecycle.restartDesktopClient("P2P transport wedged");
  }, [lifecycle, recovery, runtimeHealth, sending, starting, stopping]);
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
  const firstNode = useStore(stores.fleet, (state) => {
    const first = state.nodeKeys[0];
    return first ? (state.nodes[first]?.agentDid ?? null) : null;
  });
  const node = useStore(stores.fleet, (state) =>
    agentDid ? (state.nodes[agentDid] ?? null) : null,
  );

  // Snapshot absence is not an explicit navigation intent. Preserve an
  // existing principal selection while bounded observations catch up; only
  // initialize an empty selection through the route owner. The route's
  // effect runs first in the same commit (it is a child's), so the store,
  // not this render's value, says whether a node is already selected.
  useEffect(() => {
    if (!store.getState().agentDid && firstNode) actions.selectAgent(firstNode);
  }, [actions, agentDid, firstNode, store]);

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
  const clientAvailable = useStore(stores.client, (state) =>
    Boolean(state.snapshot?.client),
  );
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

/** The local workflow follows what the transcript shows once it shows it,
    and a submission that ended without one is released. */
function useWorkflowReconcile({ stores, view }: Pick<DesktopApp, "stores" | "view">) {
  const [setLocalWorkflow] = useState(() => setterOf(stores.chat, "localWorkflow"));
  const projected = useStore(view, (state) => state.shellProjection.workflow);
  const submitting = useStore(
    stores.chat,
    (state) => state.localWorkflow.kind === "submittingRequest" && !state.sending,
  );
  useEffect(() => {
    setLocalWorkflow((current) => reconcileProjectedWorkflow(current, projected));
  }, [projected, setLocalWorkflow]);
  useEffect(() => {
    if (submitting) setLocalWorkflow({ kind: "ready" });
  }, [submitting, setLocalWorkflow]);
}
