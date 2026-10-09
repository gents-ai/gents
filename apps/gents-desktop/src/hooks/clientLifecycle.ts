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
import {
  delay,
  logShellEvent,
  shouldAutoRestartP2P,
  timingConfig,
} from "./desktopShellRuntime";
import { createSnapshotPublicationOwner } from "./desktopSnapshotPublication";
import { applyFleetSnapshot, equal, shareUnchanged } from "./fleetStore";
import type { LocalServerActions } from "./localServer";
import { writeSession } from "./sessionStore";
import type { ShellStores } from "./shellProjection";
import { createIncompatibleHomeOps } from "./useIncompatibleHome";
import { clientStatus } from "./clientStore";

type ClientLifecycleParams = {
  api: DesktopApiAdapter;
  /** the local server's owner, which startup observes and restarts through */
  localServer: Pick<LocalServerActions, "restoreLocalServer" | "restartLocalServer">;
  supportsManagedServer: boolean;
  stores: ShellStores;
  /** reads the selected session again after a restart */
  refreshSession: (sessionId: string | null) => Promise<DesktopSessionSnapshot | null>;
};

/** What automatic recovery remembers between observations of the client. */
export type ClientRecovery = {
  /** startup has already tried to start the client once */
  autostartAttempted: boolean;
  autoRestartInFlight: boolean;
  lastP2PAutoRestartAt: number | null;
  lastObservedP2PHealth: P2PHealth | null;
};

/**
 * The desktop client's process lifecycle: startup and the managed server,
 * snapshot reads published to the client and fleet stores, single-flight
 * starts and bounded restarts. Made once; each function reads the stores
 * when it runs. Failures here are the client's own state, shown in the
 * banner through the client store's error.
 */
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
    /* by key first, so a screen reading the fleet sees the same read. A
     read that changed nothing keeps the snapshot it repeats, so no one
     reading the client is notified either. */
    const fleetBefore = fleet.getState();
    applyFleetSnapshot(fleet, next);
    const before = client.getState().snapshot;
    const unchanged =
      before !== null &&
      fleet.getState() === fleetBefore &&
      equal(withoutDeployments(before), withoutDeployments(next));
    /* what did not change keeps its object, so a screen reading one part
       (sync health, the bootstrap) is not notified when another changed */
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
    // Mutation payloads can predate reads issued while they were pending.
    // Observe committed state after acceptance; failed writes don't revoke reads.
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

  /* Automatic recovery, where this window owns it: asked after every read, a
     repeated one included (startup run again reads the same stopped client),
     and again when a start, stop or send that held it back ends. */
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

  /* A health seen while a restart could not run is left unobserved, so the
     restart is weighed against it once what held it back ends. */
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
      // The shared owner already published the exact bridge error.
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
        // A legacy or broken ~/.gents must not block first-run setup or
        // already-saved remote peers. Surface the error after the app is up.
        localServerAvailable = false;
        setError(error instanceof Error ? error.message : String(error));
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
    if (!status?.agentName || !status.effectiveToolCeiling || !api.restartManagedServer)
      return;
    const agentName = status.agentName;
    const authority = {
      toolCeiling: status.effectiveToolCeiling,
      toolRoot: status.effectiveToolRoot,
    };
    setStarting(true);
    setError(null);
    setStartupPhase("checking-managed-server");
    try {
      await localServer.restartLocalServer(agentName, authority);
      await initializeDesktop();
    } catch (error) {
      setError(error instanceof Error ? error.message : String(error));
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
      /* cleared first: ending the start and stop asks recovery again, which
         weighs a wedged transport seen during this restart */
      recovery.autoRestartInFlight = false;
      setStopping(false);
      setStarting(false);
    }
  }

  return {
    recovery,
    clientAutostarts,
    home,
    /** Shows, or clears with null, the client's own failure in the banner. */
    setError,
    /** Reads the client and publishes it to the client and fleet stores;
        only the newest read issued publishes, and a read that changed
        nothing notifies no one. */
    refreshSnapshot,
    /** Runs a write, then reads the client again; the write's result is
        returned whether or not that read succeeds. */
    mutateSnapshot,
    /** Starts the client, sharing one start among every caller while it is
        in flight. A failure is shown in the banner and rethrown. */
    ensureDesktopClientStarted,
    /** Starts the client and swallows its failure, which the banner shows. */
    startClient,
    /** Startup: restores the managed server where this window owns it, then
        reads the client. One run at a time; a second call joins it. */
    initializeDesktop,
    /** Stops waiting for the managed server and continues startup without
        it. */
    skipManagedServerWait,
    /** Restarts a managed server whose start failed, with the agent and
        authority that start reported, then runs startup again. */
    restartManagedServer,
    /** Whether this window owns a managed server, which startup checks first. */
    supportsManagedServer,
    /** Runs startup again from the beginning. */
    retryStartup,
    /** Stops and starts the client, a bounded number of times, then reads
        the session the person still has open. One restart at a time. */
    restartDesktopClient,
  };
}

/* the read without its deployments, which the fleet store compares by key */
function withoutDeployments(snapshot: DesktopClientSnapshot) {
  return snapshot.client
    ? { ...snapshot, client: { ...snapshot.client, deployments: null } }
    : snapshot;
}
