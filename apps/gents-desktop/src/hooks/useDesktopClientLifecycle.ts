import { useRef, useState, type MutableRefObject, type SetStateAction } from "react";

import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  P2PHealth,
} from "@source-inc/gents-desktop-client";
import { delay, logShellEvent, timingConfig } from "./desktopShellRuntime";

export type DesktopStartupPhase =
  | "loading-configuration"
  | "starting-client"
  | "configuration-error"
  | "client-error"
  | "ready";

type ClientLifecycleOptions = {
  api: DesktopApiAdapter;
  refreshSession: (sessionId: string | null) => Promise<DesktopSessionSnapshot | null>;
  selectedSessionIdRef: MutableRefObject<string | null>;
  setError: (error: string | null) => void;
  setSession: (next: SetStateAction<DesktopSessionSnapshot | null>) => void;
};

/** Own desktop process startup, snapshot reads, and bounded restart recovery. */
export function useDesktopClientLifecycle({
  api,
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
  const snapshotRefreshSeq = useRef(0);
  const startupPhaseRef = useRef<DesktopStartupPhase>("loading-configuration");
  const startClientInFlight = useRef<Promise<DesktopClientSnapshot | null> | null>(
    null,
  );
  const [snapshot, setSnapshot] = useState<DesktopClientSnapshot | null>(null);
  const [startupPhase, setStartupPhaseState] = useState<DesktopStartupPhase>(
    "loading-configuration",
  );
  const [loading, setLoading] = useState(true);
  const [starting, setStarting] = useState(false);
  const [stopping, setStopping] = useState(false);

  function setStartupPhase(next: DesktopStartupPhase) {
    startupPhaseRef.current = next;
    setStartupPhaseState(next);
  }

  async function refreshSnapshot() {
    const refreshSeq = snapshotRefreshSeq.current + 1;
    snapshotRefreshSeq.current = refreshSeq;
    const resolvingConfiguration = startupPhaseRef.current === "loading-configuration";
    setLoading(true);
    try {
      const next = await api.fetchDesktopSnapshot();
      if (snapshotRefreshSeq.current === refreshSeq) {
        setSnapshot(next);
        setError(null);
        if (resolvingConfiguration) {
          setStartupPhase(
            next.client || next.bootstrap.savedPeers.length === 0
              ? "ready"
              : "starting-client",
          );
        }
      }
    } catch (error) {
      if (snapshotRefreshSeq.current === refreshSeq) {
        setError(String(error));
        if (resolvingConfiguration) setStartupPhase("configuration-error");
      }
    } finally {
      if (snapshotRefreshSeq.current === refreshSeq) setLoading(false);
    }
  }

  async function ensureDesktopClientStarted(): Promise<DesktopClientSnapshot | null> {
    if (startClientInFlight.current) return startClientInFlight.current;
    setStarting(true);
    setError(null);
    const pending = (async () => {
      let started = false;
      try {
        const next = await api.startDesktopClient();
        setSnapshot(next);
        started = true;
        return next;
      } catch (error) {
        setError(String(error));
        return null;
      } finally {
        if (startupPhaseRef.current === "starting-client") {
          setStartupPhase(started ? "ready" : "client-error");
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

  async function onRetryStartup() {
    autostartAttempted.current = false;
    setStartupPhase("loading-configuration");
    await refreshSnapshot();
  }

  async function restartDesktopClient(reason: string) {
    if (autoRestartInFlight.current) return;
    autoRestartInFlight.current = true;
    const sessionId = selectedSessionIdRef.current;
    logShellEvent(`restart begin reason="${reason}" sessionId=${sessionId ?? "none"}`);
    setStopping(true);
    setStarting(true);
    setError(null);
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
      setSnapshot(next);
      if (sessionId) await refreshSession(sessionId);
      else setSession(null);
      logShellEvent(`restart complete reason="${reason}"`);
    } catch (error) {
      logShellEvent(`restart failed reason="${reason}" error=${String(error)}`);
      setError(`desktop client restart failed after ${reason}: ${String(error)}`);
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
    setSnapshot,
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
