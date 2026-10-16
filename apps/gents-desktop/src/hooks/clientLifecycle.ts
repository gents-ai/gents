import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  P2PHealth,
} from "@source-inc/gents-desktop-client";

import {
  projectStartupPhaseAfterSnapshot,
  shouldAutoStartDesktopClient,
  type DesktopStartupPhase,
} from "../lib/loadingStatus";
import { ManagedServerStartupError } from "../lib/managedServerStartup";
import { singleFlight } from "../lib/reads";
import { isMobileTauriShell, ownsAutomaticRecovery } from "../lib/shellPlatform";
import { timingConfig } from "./timing";
import { createSnapshotPublicationOwner } from "./snapshotPublication";
import { applyFleetSnapshot, equal, shareUnchanged } from "./fleetStore";
import type { LocalServerActions } from "./localServer";
import { writeSession } from "./sessionStore";
import type { ShellStores } from "./shellProjection";
import { createIncompatibleHomeOps } from "./useIncompatibleHome";
import { clientStatus } from "./clientStore";
import { errorText } from "../lib/errorText";

function shouldAutoRestartP2P(
  previous: P2PHealth | null,
  next: P2PHealth | null,
  lastAttemptAt: number | null,
  now: number,
  cooldownMs: number,
) {
  if (!next || next.status !== "wedged") {
    return false;
  }

  if (lastAttemptAt !== null && now - lastAttemptAt < cooldownMs) {
    return false;
  }

  if (!previous) {
    return true;
  }

  return (
    previous.status !== "wedged" ||
    previous.consecutiveFailures !== next.consecutiveFailures ||
    previous.lastError !== next.lastError
  );
}

async function delay(ms: number) {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

export function logShellEvent(message: string) {
  console.info(`[live-tauri-shell] ${message}`);
}

type ClientLifecycleParams = {
  api: DesktopApiAdapter;
  localServer: Pick<LocalServerActions, "restoreLocalServer" | "restartLocalServer">;
  supportsManagedServer: boolean;
  stores: ShellStores;
  refreshSession: (sessionId: string | null) => Promise<DesktopSessionSnapshot | null>;
};

export type ClientRecovery = {
  autostartAttempted: boolean;
  autoRestartInFlight: boolean;
  lastP2PAutoRestartAt: number | null;
  lastObservedP2PHealth: P2PHealth | null;
};

export function createClientLifecycle({
  api,
  localServer,
  supportsManagedServer,
  stores,
  refreshSession,
}: ClientLifecycleParams) {
  const { client, fleet } = stores;
  const setError = (error: string | null) => clientStatus.setError(client, error);
  const setStarting = (starting: boolean) => clientStatus.setStarting(client, starting);
  const setStopping = (stopping: boolean) => client.setState({ stopping });
  const setManagedServerFailure = (
    managedServerFailure: ManagedServerStartupError | null,
  ) => client.setState({ managedServerFailure });
  const recovery: ClientRecovery = {
    autostartAttempted: false,
    autoRestartInFlight: false,
    lastP2PAutoRestartAt: null,
    lastObservedP2PHealth: null,
  };
  let localServerAvailable: boolean | null = null;
  let managedServerWaitAbort: AbortController | null = null;

  function clientAutostarts(next: DesktopClientSnapshot) {
    return shouldAutoStartDesktopClient(next, localServerAvailable, {
      mobile: isMobileTauriShell(),
    });
  }

  const publication = createSnapshotPublicationOwner((next) => {
    const fleetBefore = fleet.getState();
    applyFleetSnapshot(fleet, next);
    const before = client.getState().snapshot;
    const unchanged =
      before !== null &&
      fleet.getState() === fleetBefore &&
      equal(withoutDeployments(before), withoutDeployments(next));
    if (!unchanged) client.setState({ snapshot: shareUnchanged(before, next) });
    resolveStartupPhase(next);
    recover();
  });
  const home = createIncompatibleHomeOps({
    api,
    client,
    setError,
    startFresh: () => initializeDesktop(),
  });
  function setStartupPhase(next: DesktopStartupPhase) {
    client.setState({ startupPhase: next });
  }

  function resolveStartupPhase(next: DesktopClientSnapshot) {
    const phase = projectStartupPhaseAfterSnapshot(
      client.getState().startupPhase,
      Boolean(next.client),
      !clientAutostarts(next),
    );
    if (phase !== client.getState().startupPhase) setStartupPhase(phase);
  }

  async function refreshSnapshot() {
    const publish = publication.begin();
    try {
      const next = await api.fetchDesktopSnapshot();
      if (publish.publish(next)) {
        setError(null);
      }
    } catch (error) {
      if (!publish.isCurrent()) {
        return;
      }
      setError(String(error));
      if (client.getState().startupPhase === "loading-configuration") {
        setStartupPhase("configuration-error");
      } else if (client.getState().startupPhase === "starting-client") {
        setStartupPhase("client-error");
      }
    }
  }

  async function mutateSnapshot<T>(operation: () => Promise<T>): Promise<T> {
    const accepted = await operation();
    await refreshSnapshot();
    return accepted;
  }

  const ensureDesktopClientStarted = singleFlight(async () => {
    setStarting(true);
    setError(null);
    const isCurrent = publication.checkpoint();
    try {
      return await mutateSnapshot(() => api.startDesktopClient());
    } catch (error) {
      if (isCurrent() || !publication.snapshot?.client) {
        setError(String(error));
        if (client.getState().startupPhase === "starting-client") {
          setStartupPhase("client-error");
        }
        await home.adopt(error);
      }
      throw error;
    } finally {
      setStarting(false);
    }
  });

  function recover() {
    if (!ownsAutomaticRecovery()) return;
    const { snapshot, starting, stopping } = client.getState();
    const held = starting || stopping || stores.chat.getState().sending;
    if (!snapshot) return;
    if (!snapshot.client) {
      recovery.lastObservedP2PHealth = null;
      if (held || !clientAutostarts(snapshot) || recovery.autostartAttempted) return;
      recovery.autostartAttempted = true;
      void startClient();
      return;
    }
    restartIfWedged(snapshot.client.p2pHealth ?? null, held);
  }

  function restartIfWedged(health: P2PHealth | null, held: boolean) {
    if (!health || health.status === "healthy") {
      recovery.lastObservedP2PHealth = health;
      if (health) recovery.lastP2PAutoRestartAt = null;
      return;
    }
    if (held || recovery.autoRestartInFlight) return;
    const previous = recovery.lastObservedP2PHealth;
    recovery.lastObservedP2PHealth = health;
    if (
      !shouldAutoRestartP2P(
        previous,
        health,
        recovery.lastP2PAutoRestartAt,
        Date.now(),
        timingConfig().p2pAutoRestartCooldownMs,
      )
    )
      return;
    recovery.lastP2PAutoRestartAt = Date.now();
    logShellEvent(
      `auto restart requested reason="P2P transport wedged" status=${health.status} failures=${health.consecutiveFailures}`,
    );
    void restartDesktopClient("P2P transport wedged");
  }

  client.subscribe((state, prev) => {
    if ((prev.starting && !state.starting) || (prev.stopping && !state.stopping))
      recover();
  });
  stores.chat.subscribe((state, prev) => {
    if (prev.sending && !state.sending) recover();
  });

  async function startClient() {
    try {
      await ensureDesktopClientStarted();
    } catch {
    }
  }

  const initializeDesktop = singleFlight(async (): Promise<void> => {
    recovery.autostartAttempted = false;
    if (supportsManagedServer && ownsAutomaticRecovery()) {
      setStartupPhase("checking-managed-server");
      const abort = new AbortController();
      managedServerWaitAbort = abort;
      setManagedServerFailure(null);
      try {
        localServerAvailable = await localServer.restoreLocalServer(abort.signal);
      } catch (error) {
        localServerAvailable = false;
        setError(errorText(error));
        if (await home.adopt(error)) {
          setStartupPhase("managed-server-error");
          return;
        }
        if (error instanceof ManagedServerStartupError) {
          setManagedServerFailure(error);
          setStartupPhase("managed-server-error");
          return;
        }
      }
    }
    setStartupPhase("loading-configuration");
    await refreshSnapshot();
  });

  function skipManagedServerWait() {
    if (initializeDesktop.running) {
      managedServerWaitAbort?.abort();
      return;
    }
    localServerAvailable = false;
    setManagedServerFailure(null);
    setError(null);
    setStartupPhase("loading-configuration");
    void refreshSnapshot();
  }

  async function restartManagedServer() {
    const status = client.getState().managedServerFailure?.status;
    if (!status?.nodeName || !status.effectiveToolCeiling || !api.restartManagedServer)
      return;
    const nodeName = status.nodeName;
    const authority = {
      toolCeiling: status.effectiveToolCeiling,
      toolRoot: status.effectiveToolRoot,
    };
    setStarting(true);
    setError(null);
    setStartupPhase("checking-managed-server");
    try {
      await localServer.restartLocalServer(nodeName, authority);
      await initializeDesktop();
    } catch (error) {
      setError(errorText(error));
      setStartupPhase("managed-server-error");
      await home.adopt(error);
    } finally {
      setStarting(false);
    }
  }

  async function retryStartup() {
    await initializeDesktop();
  }

  async function restartDesktopClient(reason: string) {
    if (recovery.autoRestartInFlight) return;
    recovery.autoRestartInFlight = true;
    const sessionId = stores.selection.getState().sessionId;
    logShellEvent(`restart begin reason="${reason}" sessionId=${sessionId ?? "none"}`);
    setStopping(true);
    setStarting(true);
    setError(null);
    const isCurrent = publication.checkpoint();
    try {
      let next: DesktopClientSnapshot | null = null;
      for (
        let attempt = 1;
        attempt <= timingConfig().clientRestartMaxAttempts;
        attempt += 1
      ) {
        try {
          logShellEvent(`restart attempt=${attempt} phase=shutdown`);
          await api.shutdownDesktopClient();
          logShellEvent(`restart attempt=${attempt} phase=start`);
          next = await api.startDesktopClient();
          break;
        } catch (error) {
          logShellEvent(`restart attempt=${attempt} failed error=${String(error)}`);
          if (attempt === timingConfig().clientRestartMaxAttempts) throw error;
          await delay(timingConfig().clientRestartBackoffMs);
        }
      }
      if (!next) throw new Error("desktop restart returned no snapshot");
      await refreshSnapshot();
      if (stores.selection.getState().sessionId === sessionId) {
        if (sessionId) await refreshSession(sessionId);
        else writeSession(stores.session, null);
      }
      logShellEvent(`restart complete reason="${reason}"`);
    } catch (error) {
      logShellEvent(`restart failed reason="${reason}" error=${String(error)}`);
      if (isCurrent() || !publication.snapshot?.client) {
        setError(`desktop client restart failed after ${reason}: ${String(error)}`);
        await home.adopt(error);
      }
    } finally {
      recovery.autoRestartInFlight = false;
      setStopping(false);
      setStarting(false);
    }
  }

  return {
    recovery,
    clientAutostarts,
    home,
    setError,
    refreshSnapshot,
    mutateSnapshot,
    ensureDesktopClientStarted,
    startClient,
    initializeDesktop,
    skipManagedServerWait,
    restartManagedServer,
    supportsManagedServer,
    retryStartup,
    restartDesktopClient,
  };
}

function withoutDeployments(snapshot: DesktopClientSnapshot) {
  return snapshot.client
    ? { ...snapshot, client: { ...snapshot.client, deployments: null } }
    : snapshot;
}
