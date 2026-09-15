import {
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
import type { DesktopStartupPhase } from "../lib/loadingStatus";
import { restoreManagedServer } from "./managedServerLifecycle";
import { createSnapshotPublicationOwner } from "./desktopSnapshotPublication";

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
  const autoRestartInFlight = useRef(false);
  const lastP2PAutoRestartAt = useRef<number | null>(null);
  const lastObservedP2PHealth = useRef<P2PHealth | null>(null);
  const snapshotRefreshGeneration = useRef(0);
  const initialStartupPhase: DesktopStartupPhase = supportsManagedServer
    ? "checking-managed-server"
    : "loading-configuration";
  const startupPhaseRef = useRef<DesktopStartupPhase>(initialStartupPhase);
  const startClientInFlight = useRef<Promise<DesktopClientSnapshot | null> | null>(
    null,
  );
  const initializationInFlight = useRef<Promise<void> | null>(null);
  const [snapshot, setSnapshot] = useState<DesktopClientSnapshot | null>(null);
  const snapshotPublicationRef = useRef<
    ReturnType<typeof createSnapshotPublicationOwner> | undefined
  >(undefined);
  snapshotPublicationRef.current ??= createSnapshotPublicationOwner((next) => {
    setSnapshot(next);
    snapshotRefreshGeneration.current += 1;
    setLoading(false);
    resolveStartupPhase(next);
  });
  const [startupPhase, setStartupPhaseState] =
    useState<DesktopStartupPhase>(initialStartupPhase);
  const [loading, setLoading] = useState(true);
  const [starting, setStarting] = useState(false);
  const [stopping, setStopping] = useState(false);

  function setStartupPhase(next: DesktopStartupPhase) {
    startupPhaseRef.current = next;
    setStartupPhaseState(next);
  }

  function resolveStartupPhase(next: DesktopClientSnapshot) {
    if (
      startupPhaseRef.current !== "loading-configuration" &&
      startupPhaseRef.current !== "starting-client"
    ) {
      return;
    }
    setStartupPhase(
      next.client ||
        (!next.bootstrap.clientStateExists && next.bootstrap.savedPeers.length === 0)
        ? "ready"
        : "starting-client",
    );
  }

  function beginSnapshotPublication() {
    return snapshotPublicationRef.current!.begin();
  }

  async function refreshSnapshot() {
    snapshotRefreshGeneration.current += 1;
    const refreshGeneration = snapshotRefreshGeneration.current;
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
      }
    } finally {
      if (snapshotRefreshGeneration.current === refreshGeneration) setLoading(false);
    }
  }

  async function ensureDesktopClientStarted(): Promise<DesktopClientSnapshot | null> {
    if (startClientInFlight.current) return startClientInFlight.current;
    setStarting(true);
    setError(null);
    const pending = (async () => {
      const publish = beginSnapshotPublication();
      try {
        const next = await api.startDesktopClient();
        publish.publish(next);
        return next;
      } catch (error) {
        if (publish.isCurrent()) setError(String(error));
        return null;
      } finally {
        if (publish.isCurrent() && startupPhaseRef.current === "starting-client") {
          setStartupPhase("client-error");
        }
        startClientInFlight.current = null;
        setStarting(false);
      }
    })();
    startClientInFlight.current = pending;
    return pending;
  }

  async function onStartClient() {
    await ensureDesktopClientStarted();
  }

  function initializeDesktop(): Promise<void> {
    if (initializationInFlight.current) return initializationInFlight.current;
    const pending = (async () => {
      autostartAttempted.current = false;
      if (supportsManagedServer) {
        setStartupPhase("checking-managed-server");
        try {
          localServerAvailable.current = await restoreManagedServer(api);
        } catch (error) {
          // A legacy or broken ~/.gents must not block first-run setup or
          // already-saved remote peers. Surface the error after the shell is up.
          localServerAvailable.current = false;
          setError(String(error));
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
    const publish = beginSnapshotPublication();
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
      const published = publish.publish(next);
      if (published && sessionId) await refreshSession(sessionId);
      else if (published && selectedSessionIdRef.current === sessionId)
        setSession(null);
      logShellEvent(`restart complete reason="${reason}"`);
    } catch (error) {
      logShellEvent(`restart failed reason="${reason}" error=${String(error)}`);
      if (publish.isCurrent()) {
        setError(`desktop client restart failed after ${reason}: ${String(error)}`);
      }
    } finally {
      setStopping(false);
      setStarting(false);
      autoRestartInFlight.current = false;
    }
  }

  return {
    autostartAttempted,
    localServerAvailable,
    autoRestartInFlight,
    lastP2PAutoRestartAt,
    lastObservedP2PHealth,
    snapshot,
    beginSnapshotPublication,
    startupPhase,
    loading,
    starting,
    setStarting,
    stopping,
    refreshSnapshot,
    ensureDesktopClientStarted,
    onStartClient,
    onRetryStartup,
    restartDesktopClient,
  };
}
