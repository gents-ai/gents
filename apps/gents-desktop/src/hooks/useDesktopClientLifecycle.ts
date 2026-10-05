import { useCallback, useEffect, useRef, useState, type SetStateAction } from "react";

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  P2PHealth,
} from "@source-inc/gents-desktop-client";
import { delay, logShellEvent, timingConfig } from "./desktopShellRuntime";
import {
  projectStartupPhaseAfterSnapshot,
  shouldAutoStartDesktopClient,
  type DesktopStartupPhase,
} from "../lib/loadingStatus";
import { restoreManagedServer } from "./managedServerLifecycle";
import {
  ManagedServerStartupError,
  observeManagedServerOperation,
} from "../lib/managedServerStartup";
import { isMobileTauriShell, ownsAutomaticRecovery } from "../lib/shellPlatform";
import { createSnapshotPublicationOwner } from "./desktopSnapshotPublication";
import { createIncompatibleHomeOps, useIncompatibleHome } from "./useIncompatibleHome";
import type { SelectionStore } from "./selectionStore";
import { useStore } from "zustand";
import { useShallow } from "zustand/react/shallow";

import { clientSetter, type ClientStore } from "./clientStore";
import { applyFleetSnapshot, equal, type FleetStore } from "./fleetStore";

export type { DesktopStartupPhase } from "../lib/loadingStatus";

type ClientLifecycleOptions = {
  api: DesktopApiAdapter;
  supportsManagedServer: boolean;
  refreshSession: (sessionId: string | null) => Promise<DesktopSessionSnapshot | null>;
  /** the selection, read when a restart finishes */
  store: SelectionStore;
  /** where each read is published: the client as read, and its fleet by key */
  client: ClientStore;
  fleet: FleetStore;
  setError: (error: string | null) => void;
  setSession: (next: SetStateAction<DesktopSessionSnapshot | null>) => void;
};

/** Own desktop process startup, snapshot reads, and bounded restart recovery. */
export function useDesktopClientLifecycle({
  api,
  supportsManagedServer,
  refreshSession,
  store,
  client,
  fleet,
  setError,
  setSession,
}: ClientLifecycleOptions) {
  const autostartAttempted = useRef(false);
  const localServerAvailable = useRef<boolean | null>(null);
  const clientAutostarts = useCallback(
    (next: DesktopClientSnapshot) =>
      shouldAutoStartDesktopClient(next, localServerAvailable.current, {
        mobile: isMobileTauriShell(),
      }),
    [],
  );
  const autoRestartInFlight = useRef(false);
  const lastP2PAutoRestartAt = useRef<number | null>(null);
  const lastObservedP2PHealth = useRef<P2PHealth | null>(null);
  const startClientInFlight = useRef<Promise<DesktopClientSnapshot> | null>(null);
  const initializationInFlight = useRef<Promise<void> | null>(null);
  const snapshot = useStore(client, (state) => state.snapshot);
  /* startup's state lives in the client store: functions read it there when
     they run, and screens select it */
  const { startupPhase, starting, stopping, managedServerWait } = useStore(
    client,
    useShallow((state) => ({
      startupPhase: state.startupPhase,
      starting: state.starting,
      stopping: state.stopping,
      managedServerWait: state.managedServerWait,
    })),
  );
  const [setStarting] = useState(() => clientSetter(client, "starting"));
  const [setStopping] = useState(() => clientSetter(client, "stopping"));
  const [setManagedServerWait] = useState(() =>
    clientSetter(client, "managedServerWait"),
  );
  const managedServerWaitAbort = useRef<AbortController | null>(null);
  const managedServerFailure = useStore(client, (state) => state.managedServerFailure);
  const [setManagedServerFailure] = useState(() =>
    clientSetter(client, "managedServerFailure"),
  );
  const startupDiagnosticsHint = useStore(
    client,
    (state) => state.startupDiagnosticsHint,
  );
  const [setStartupDiagnosticsHint] = useState(() =>
    clientSetter(client, "startupDiagnosticsHint"),
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
        if (current) setStartupDiagnosticsHint(next.bootstrap.diagnosticsHint || null);
      })
      .catch(() => {});
    return () => {
      current = false;
    };
  }, [api, managedServerFailed, startupDiagnosticsHint, snapshot]);

  /* the lifecycle's functions, made once from inputs that keep their
     identity for the app's life: each reads the stores and refs when it
     runs, so none holds a stale copy and its identity never changes */
  const [ops] = useState(() => {
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
      if (!unchanged) client.setState({ snapshot: next });
      resolveStartupPhase(next);
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

    async function ensureDesktopClientStarted(): Promise<DesktopClientSnapshot> {
      if (startClientInFlight.current) return startClientInFlight.current;
      setStarting(true);
      setError(null);
      const pending = (async () => {
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
          startClientInFlight.current = null;
          setStarting(false);
        }
      })();
      startClientInFlight.current = pending;
      return pending;
    }

    async function onStartClient() {
      try {
        await ensureDesktopClientStarted();
      } catch {
        // The shared owner already published the exact bridge error.
      }
    }

    function initializeDesktop(): Promise<void> {
      if (initializationInFlight.current) return initializationInFlight.current;
      const pending = (async () => {
        autostartAttempted.current = false;
        if (supportsManagedServer && ownsAutomaticRecovery()) {
          setStartupPhase("checking-managed-server");
          const abort = new AbortController();
          managedServerWaitAbort.current = abort;
          setManagedServerFailure(null);
          try {
            localServerAvailable.current = await restoreManagedServer(api, {
              onWait: setManagedServerWait,
              signal: abort.signal,
            });
          } catch (error) {
            // A legacy or broken ~/.gents must not block first-run setup or
            // already-saved remote peers. Surface the error after the shell is up.
            localServerAvailable.current = false;
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
      })().finally(() => {
        if (initializationInFlight.current === pending) {
          initializationInFlight.current = null;
        }
      });
      initializationInFlight.current = pending;
      return pending;
    }

    function onSkipManagedServerWait() {
      if (initializationInFlight.current) {
        managedServerWaitAbort.current?.abort();
        return;
      }
      localServerAvailable.current = false;
      setManagedServerFailure(null);
      setError(null);
      setStartupPhase("loading-configuration");
      void refreshSnapshot();
    }

    async function onRestartManagedServer() {
      const status = client.getState().managedServerFailure?.status;
      if (
        !status?.agentName ||
        !status.effectiveToolCeiling ||
        !api.restartManagedServer
      )
        return;
      const restartManagedServer = api.restartManagedServer;
      const agentName = status.agentName;
      const authority = {
        toolCeiling: status.effectiveToolCeiling,
        toolRoot: status.effectiveToolRoot,
      };
      setStarting(true);
      setError(null);
      setStartupPhase("checking-managed-server");
      try {
        await observeManagedServerOperation(
          api,
          () => restartManagedServer(agentName, authority),
          setManagedServerWait,
        );
        await initializeDesktop();
      } catch (error) {
        setError(error instanceof Error ? error.message : String(error));
        setStartupPhase("managed-server-error");
        await home.adopt(error);
      } finally {
        setStarting(false);
      }
    }

    async function onRetryStartup() {
      await initializeDesktop();
    }

    async function restartDesktopClient(reason: string) {
      if (autoRestartInFlight.current) return;
      autoRestartInFlight.current = true;
      const sessionId = store.getState().sessionId;
      logShellEvent(
        `restart begin reason="${reason}" sessionId=${sessionId ?? "none"}`,
      );
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
        if (store.getState().sessionId === sessionId) {
          if (sessionId) await refreshSession(sessionId);
          else setSession(null);
        }
        logShellEvent(`restart complete reason="${reason}"`);
      } catch (error) {
        logShellEvent(`restart failed reason="${reason}" error=${String(error)}`);
        if (isCurrent() || !publication.snapshot?.client) {
          setError(`desktop client restart failed after ${reason}: ${String(error)}`);
          await home.adopt(error);
        }
      } finally {
        setStopping(false);
        setStarting(false);
        autoRestartInFlight.current = false;
      }
    }

    return {
      publication,
      home,
      refreshSnapshot,
      mutateSnapshot,
      ensureDesktopClientStarted,
      onStartClient,
      initializeDesktop,
      onSkipManagedServerWait,
      onRestartManagedServer,
      onRetryStartup,
      restartDesktopClient,
    };
  });
  const {
    refreshSnapshot,
    mutateSnapshot,
    ensureDesktopClientStarted,
    onStartClient,
    onSkipManagedServerWait,
    onRestartManagedServer,
    onRetryStartup,
    restartDesktopClient,
  } = ops;
  const incompatibleHome = useIncompatibleHome(client, ops.home);

  useEffect(() => {
    void ops.initializeDesktop();
  }, [ops]);

  return {
    autostartAttempted,
    clientAutostarts,
    autoRestartInFlight,
    lastP2PAutoRestartAt,
    lastObservedP2PHealth,
    snapshot,
    mutateSnapshot,
    startupPhase,
    starting,
    setStarting,
    stopping,
    refreshSnapshot,
    ensureDesktopClientStarted,
    onStartClient,
    onRetryStartup,
    incompatibleHome,
    managedServerWait,
    diagnosticsHint: snapshot?.bootstrap.diagnosticsHint || startupDiagnosticsHint,
    onSkipManagedServerWait,
    canRestartManagedServer: Boolean(
      managedServerFailure?.status.agentName &&
      managedServerFailure.status.effectiveToolCeiling &&
      api.restartManagedServer,
    ),
    onRestartManagedServer,
    restartDesktopClient,
  };
}

/* the read without its deployments, which the fleet store compares by key */
function withoutDeployments(snapshot: DesktopClientSnapshot) {
  return snapshot.client
    ? { ...snapshot, client: { ...snapshot.client, deployments: null } }
    : snapshot;
}
