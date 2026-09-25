import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type MutableRefObject,
  type SetStateAction,
} from "react";

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
  type ManagedServerWait,
} from "../lib/managedServerStartup";
import { isMobileTauriShell, ownsAutomaticRecovery } from "../lib/shellPlatform";
import { createSnapshotPublicationOwner } from "./desktopSnapshotPublication";
import { useIncompatibleHome } from "./useIncompatibleHome";

export type { DesktopStartupPhase } from "../lib/loadingStatus";

type ClientLifecycleOptions = {
  api: DesktopApiAdapter;
  supportsManagedServer: boolean;
  refreshSession: (sessionId: string | null) => Promise<DesktopSessionSnapshot | null>;
  selectedSessionIdRef: MutableRefObject<string | null>;
  setError: (error: string | null) => void;
  setSession: (next: SetStateAction<DesktopSessionSnapshot | null>) => void;
};

/** Own desktop process startup, snapshot reads, and bounded restart recovery. */
export function useDesktopClientLifecycle({
  api,
  supportsManagedServer,
  refreshSession,
  selectedSessionIdRef,
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
  const initialStartupPhase: DesktopStartupPhase = supportsManagedServer
    ? "checking-managed-server"
    : "loading-configuration";
  const startupPhaseRef = useRef<DesktopStartupPhase>(initialStartupPhase);
  const startClientInFlight = useRef<Promise<DesktopClientSnapshot> | null>(null);
  const initializationInFlight = useRef<Promise<void> | null>(null);
  const [snapshot, setSnapshot] = useState<DesktopClientSnapshot | null>(null);
  const snapshotPublicationRef = useRef<
    ReturnType<typeof createSnapshotPublicationOwner> | undefined
  >(undefined);
  snapshotPublicationRef.current ??= createSnapshotPublicationOwner((next) => {
    setSnapshot(next);
    setLoading(false);
    resolveStartupPhase(next);
  });
  const [startupPhase, setStartupPhaseState] =
    useState<DesktopStartupPhase>(initialStartupPhase);
  const [loading, setLoading] = useState(true);
  const [starting, setStarting] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [managedServerWait, setManagedServerWait] = useState<ManagedServerWait | null>(
    null,
  );
  const managedServerWaitAbort = useRef<AbortController | null>(null);
  const [managedServerFailure, setManagedServerFailure] =
    useState<ManagedServerStartupError | null>(null);
  const [startupDiagnosticsHint, setStartupDiagnosticsHint] = useState<string | null>(
    null,
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
  const incompatibleHome = useIncompatibleHome({
    api,
    setError,
    startFresh: () => initializeDesktop(),
  });

  function setStartupPhase(next: DesktopStartupPhase) {
    startupPhaseRef.current = next;
    setStartupPhaseState(next);
  }

  function resolveStartupPhase(next: DesktopClientSnapshot) {
    const phase = projectStartupPhaseAfterSnapshot(
      startupPhaseRef.current,
      Boolean(next.client),
      !clientAutostarts(next),
    );
    if (phase !== startupPhaseRef.current) setStartupPhase(phase);
  }

  async function refreshSnapshot() {
    const publish = snapshotPublicationRef.current!.begin();
    setLoading(true);
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
      if (startupPhaseRef.current === "loading-configuration") {
        setStartupPhase("configuration-error");
      } else if (startupPhaseRef.current === "starting-client") {
        setStartupPhase("client-error");
      }
    } finally {
      if (publish.isCurrent()) setLoading(false);
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
      const isCurrent = snapshotPublicationRef.current!.checkpoint();
      try {
        return await mutateSnapshot(() => api.startDesktopClient());
      } catch (error) {
        if (isCurrent() || !snapshotPublicationRef.current!.snapshot?.client) {
          setError(String(error));
          if (startupPhaseRef.current === "starting-client") {
            setStartupPhase("client-error");
          }
          await incompatibleHome.adopt(error);
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
          if (await incompatibleHome.adopt(error)) {
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
    const status = managedServerFailure?.status;
    if (!status?.agentName || !status.effectiveToolCeiling || !api.restartManagedServer)
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
      await incompatibleHome.adopt(error);
    } finally {
      setStarting(false);
    }
  }

  async function onRetryStartup() {
    await initializeDesktop();
  }

  useEffect(() => {
    void initializeDesktop();
  }, []);

  async function restartDesktopClient(reason: string) {
    if (autoRestartInFlight.current) return;
    autoRestartInFlight.current = true;
    const sessionId = selectedSessionIdRef.current;
    logShellEvent(`restart begin reason="${reason}" sessionId=${sessionId ?? "none"}`);
    setStopping(true);
    setStarting(true);
    setError(null);
    const isCurrent = snapshotPublicationRef.current!.checkpoint();
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
      if (selectedSessionIdRef.current === sessionId) {
        if (sessionId) await refreshSession(sessionId);
        else setSession(null);
      }
      logShellEvent(`restart complete reason="${reason}"`);
    } catch (error) {
      logShellEvent(`restart failed reason="${reason}" error=${String(error)}`);
      if (isCurrent() || !snapshotPublicationRef.current!.snapshot?.client) {
        setError(`desktop client restart failed after ${reason}: ${String(error)}`);
        await incompatibleHome.adopt(error);
      }
    } finally {
      setStopping(false);
      setStarting(false);
      autoRestartInFlight.current = false;
    }
  }

  return {
    autostartAttempted,
    clientAutostarts,
    autoRestartInFlight,
    lastP2PAutoRestartAt,
    lastObservedP2PHealth,
    snapshot,
    mutateSnapshot,
    startupPhase,
    loading,
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
