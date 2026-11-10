import type {
  DeploymentOperationalState,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  OperationalStatus,
} from "@source-inc/gents-desktop-client";
import type { BridgeErrorCode } from "@source-inc/gents-desktop-client/generated/BridgeErrorCode";

export type DesktopStartupPhase =
  | "checking-managed-server"
  | "loading-configuration"
  | "starting-client"
  | "managed-server-error"
  | "configuration-error"
  | "client-error"
  | "ready";

export function shouldAutoStartDesktopClient(
  snapshot: DesktopClientSnapshot,
  localServerAvailable: boolean | null,
  options: { mobile?: boolean } = {},
): boolean {
  if (options.mobile) {
    return true;
  }
  return (
    snapshot.bootstrap.clientStateExists ||
    snapshot.bootstrap.savedPeers.some(
      (peer) => peer.source !== "local-standard" || localServerAvailable !== false,
    )
  );
}

export function projectStartupPhaseAfterSnapshot(
  phase: DesktopStartupPhase,
  running: boolean,
  autostartDeclined: boolean,
): DesktopStartupPhase {
  if (phase === "loading-configuration" || phase === "starting-client") {
    return running || autostartDeclined ? "ready" : "starting-client";
  }
  return phase === "client-error" && running ? "ready" : phase;
}

export type LoadingStepState = "active" | "complete" | "pending" | "error";

export type StartupLoadingStatus = {
  failed: boolean;
  title: string;
  currentLabel: string;
  managedServerState: LoadingStepState | null;
  connectionState: LoadingStepState;
  clientState: LoadingStepState;
};

export function projectStartupLoadingStatus(
  phase: Exclude<DesktopStartupPhase, "ready">,
  managedServerSupported = false,
): StartupLoadingStatus {
  switch (phase) {
    case "checking-managed-server":
      return {
        failed: false,
        title: "Starting Gents",
        currentLabel: "Starting your local node",
        managedServerState: "active",
        connectionState: "pending",
        clientState: "pending",
      };
    case "loading-configuration":
      return {
        failed: false,
        title: "Starting Gents",
        currentLabel: "Reading saved connections",
        managedServerState: managedServerSupported ? "complete" : null,
        connectionState: "active",
        clientState: "pending",
      };
    case "starting-client":
      return {
        failed: false,
        title: "Starting Gents",
        currentLabel: "Starting the secure client",
        managedServerState: managedServerSupported ? "complete" : null,
        connectionState: "complete",
        clientState: "active",
      };
    case "managed-server-error":
      return {
        failed: true,
        title: "Startup paused",
        currentLabel: "The background node could not be checked",
        managedServerState: "error",
        connectionState: "pending",
        clientState: "pending",
      };
    case "configuration-error":
      return {
        failed: true,
        title: "Startup paused",
        currentLabel: "Saved connections could not be read",
        managedServerState: managedServerSupported ? "complete" : null,
        connectionState: "error",
        clientState: "pending",
      };
    case "client-error":
      return {
        failed: true,
        title: "Startup paused",
        currentLabel: "The secure client could not start",
        managedServerState: managedServerSupported ? "complete" : null,
        connectionState: "complete",
        clientState: "error",
      };
  }
}

export type StartupStep = {
  key: "node" | "connections" | "client";
  label: string;
  doing: string;
  state: LoadingStepState;
};

export function startupSteps(
  status: StartupLoadingStatus,
  options: { remote?: boolean } = {},
): StartupStep[] {
  const node: StartupStep[] = status.managedServerState
    ? [
        options.remote
          ? {
              key: "node",
              label: "Connect to server",
              doing: "Connecting to the server",
              state: status.managedServerState,
            }
          : {
              key: "node",
              label: "Start local node",
              doing: "Starting your local node",
              state: status.managedServerState,
            },
      ]
    : [];
  return [
    ...node,
    {
      key: "connections",
      label: "Load saved connections",
      doing: "Reading saved connections",
      state: status.connectionState,
    },
    {
      key: "client",
      label: "Start secure client",
      doing: "Starting the secure client",
      state: status.clientState,
    },
  ];
}

/** How far a message into a session the node has not returned yet has got:
    the bridge derives it from the local request, the node's claim and the
    pairing, and re-reads it when any of them changes. */
export type SessionDeliveryPhase = "delivering" | "notConnected" | "accepted";

const SESSION_DELIVERY_PHASES: Partial<Record<BridgeErrorCode, SessionDeliveryPhase>> =
  {
    sessionDelivering: "delivering",
    sessionNotConnected: "notConnected",
    sessionAccepted: "accepted",
  };

export function sessionDeliveryPhase(
  code: BridgeErrorCode,
): SessionDeliveryPhase | null {
  return SESSION_DELIVERY_PHASES[code] ?? null;
}

export function isSessionDeliveryPhase(
  phase: SessionLoadState["phase"],
): phase is SessionDeliveryPhase {
  return phase === "delivering" || phase === "notConnected" || phase === "accepted";
}

export type SessionLoadState = {
  phase: "idle" | "loading" | "pending" | SessionDeliveryPhase | "loaded" | "failed";
  sessionId: string | null;
  nodeDid: string | null;
  found: boolean | null;
  error: string | null;
  /** a read of this target is running or scheduled; a waiting phase without
      one offers its action instead of waiting on a notice that may never
      come (Lean `ClientSessionLoad.phase_has_exit`) */
  reread: boolean;
};

/** The selection match every session view shares: the same session, and the
    same node unless either side names none (Lean
    `ClientSessionLoad.matchesSelection`). */
export function selectionMatches(
  selectedSessionId: string | null,
  selectedNodeDid: string | null,
  target: { sessionId: string | null; nodeDid?: string | null } | null,
): boolean {
  return (
    !!target &&
    target.sessionId === selectedSessionId &&
    (!selectedNodeDid || !target.nodeDid || target.nodeDid === selectedNodeDid)
  );
}

export type SessionLoadingLayer =
  "localDatabase" | "p2p" | "sessionSync" | "sync" | "runtime" | "inference";

export type SessionLoadingAction =
  "retryLocal" | "reloadSession" | "configureInference";

export type SessionLoadingStatus = {
  layer: SessionLoadingLayer;
  phase: "loading" | "blocked" | "failed";
  title: string;
  detail: string;
  action: SessionLoadingAction | null;
};

type SessionLoadingInput = {
  selectedSessionId: string | null;
  selectedNodeDid: string | null;
  session: Pick<DesktopSessionSnapshot, "sessionId" | "nodeDid"> | null;
  sessionLoad: SessionLoadState;
  operationalState: DeploymentOperationalState | null;
};

export function projectSessionLoadingStatus({
  selectedSessionId,
  selectedNodeDid,
  session,
  sessionLoad,
  operationalState,
}: SessionLoadingInput): SessionLoadingStatus | null {
  if (!selectedSessionId) return null;

  const sessionMatches = selectionMatches(selectedSessionId, selectedNodeDid, session);
  const loadMatches = selectionMatches(selectedSessionId, selectedNodeDid, sessionLoad);

  if (loadMatches && sessionLoad.phase === "pending" && !sessionMatches) {
    return {
      layer: "sessionSync",
      phase: "loading",
      title: "Waiting for session update",
      detail: "Waiting for the latest conversation update.",
      action: followUpAction(sessionLoad.reread),
    };
  }

  if (loadMatches && isSessionDeliveryPhase(sessionLoad.phase) && !sessionMatches) {
    return sessionDeliveryStatus(
      sessionLoad.phase,
      sessionLoad.error,
      sessionLoad.reread,
    );
  }

  if (loadMatches && sessionLoad.phase === "loading" && sessionLoad.error) {
    return {
      layer: "localDatabase",
      phase: "loading",
      title: "Retrying conversation update",
      detail: sessionMatches
        ? "The last update failed. Displayed messages may be out of date."
        : "The last read failed. Retrying this conversation.",
      action: null,
    };
  }

  if (loadMatches && sessionLoad.phase === "loading" && !sessionMatches) {
    return {
      layer: "localDatabase",
      phase: "loading",
      title: "Loading session",
      detail: "Reading saved messages from the runtime.",
      action: null,
    };
  }

  if (loadMatches && sessionLoad.phase === "failed") {
    return {
      layer: "localDatabase",
      phase: "failed",
      title: "Couldn’t load this session",
      detail: sessionLoad.error ?? "The runtime read failed.",
      action: "retryLocal",
    };
  }

  if (!loadMatches && !sessionMatches) {
    return {
      layer: "localDatabase",
      phase: "loading",
      title: "Opening session",
      detail: "Preparing the runtime session read.",
      action: null,
    };
  }

  /* the load names the selection and no read runs, yet nothing is shown: a
     writer dropped the shown session without moving the selection, so no
     read follows on its own */
  if (!sessionMatches) {
    if (loadMatches && sessionLoad.phase === "loaded" && sessionLoad.found === false)
      return null;
    return {
      layer: "localDatabase",
      phase: "loading",
      title: "Loading session",
      detail: "Reading the session from its runtime.",
      action: "retryLocal",
    };
  }

  if (operationalState?.admissionBlocker)
    return sessionStatusFromOperational(operationalState.admissionBlocker);

  return null;
}

function sessionStatusFromOperational(
  operational: OperationalStatus,
): SessionLoadingStatus {
  return {
    layer:
      operational.layer === "p2p" || operational.layer === "route"
        ? "p2p"
        : operational.layer === "sync"
          ? "sync"
          : operational.layer === "inference"
            ? "inference"
            : "runtime",
    phase: operational.kind === "blocked" ? "blocked" : "loading",
    title: operational.label,
    detail: operational.detail,
    action: operational.action,
  };
}

/* With no read running or scheduled, a waiting phase offers a fresh read of
   the selected session, the narrowest recovery the shell has. Its wording,
   and whether it should reopen the session instead, are still a product
   decision. */
function followUpAction(reread: boolean): SessionLoadingAction | null {
  return reread ? null : "retryLocal";
}

function sessionDeliveryStatus(
  phase: SessionDeliveryPhase,
  detail: string | null,
  reread: boolean,
): SessionLoadingStatus {
  switch (phase) {
    case "delivering":
      return {
        layer: "sessionSync",
        phase: "loading",
        title: "Sending message",
        detail: detail ?? "Waiting for the node to pick up the message.",
        action: followUpAction(reread),
      };
    case "notConnected":
      return {
        layer: "p2p",
        phase: "blocked",
        title: "Not connected to the node",
        detail:
          detail ??
          "The message is saved on this computer and sends when the connection returns.",
        action: followUpAction(reread),
      };
    case "accepted":
      return {
        layer: "sessionSync",
        phase: "loading",
        title: "Message picked up",
        detail:
          detail ?? "The node accepted the message. Its session is on the way back.",
        action: followUpAction(reread),
      };
  }
}
