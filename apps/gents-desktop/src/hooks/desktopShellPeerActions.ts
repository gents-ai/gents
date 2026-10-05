import { formatPeerConnectionError } from "@source-inc/gents-desktop-fleet";
import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";
import { clientSetter } from "./clientStore";
import { shownFailure } from "./desktopShellRuntime";
import type { ShellStores } from "./shellProjection";

type PeerActionParams = {
  api: DesktopApiAdapter;
  stores: ShellStores;
  /** Shared single-flight start used by autostart and peer actions. */
  ensureDesktopClientStarted: () => Promise<DesktopClientSnapshot>;
  mutateSnapshot: <T>(operation: () => Promise<T>) => Promise<T>;
  refreshSnapshot: () => Promise<void>;
  setError: (error: string | null) => void;
  selectAgent: (agentDid: string | null) => void;
};

export function createDesktopShellPeerActions({
  api,
  stores,
  ensureDesktopClientStarted,
  mutateSnapshot,
  refreshSnapshot,
  setError,
  selectAgent,
}: PeerActionParams) {
  const setStarting = clientSetter(stores.client, "starting");
  /** whether the client runs, as last read */
  const clientRuns = () => Boolean(stores.client.getState().snapshot?.client);
  async function initLocalRuntime(label?: string | null) {
    const clientWasRunning = clientRuns();
    setStarting(true);
    setError(null);
    try {
      if (clientWasRunning) {
        await mutateSnapshot(() => api.shutdownDesktopClient());
      }
      const summary = await api.initLocalStandardRuntime({
        label: label?.trim() || "Local Agent",
        dangerouslyOverwrite: false,
        reset: false,
      });
      // Init durably writes the local-standard peer entry. Restarting the
      // client is the only supported way to hydrate that trusted local route.
      await ensureDesktopClientStarted();
      selectAgent(summary.agentDid);
      return summary;
    } catch (err) {
      if (clientWasRunning) {
        try {
          await mutateSnapshot(() => api.startDesktopClient());
        } catch {
          // Preserve the provisioning error that caused the rollback.
        }
      }
      const message = formatPeerConnectionError(err, "local-runtime");
      setError(message);
      throw shownFailure(new Error(message));
    } finally {
      setStarting(false);
    }
  }

  async function fetchPeerStatus(peerId: string) {
    setError(null);
    try {
      return await api.fetchPeerStatus(peerId);
    } catch (err) {
      const message = formatPeerConnectionError(err, "peer-status");
      setError(message);
      throw shownFailure(new Error(message));
    }
  }

  async function requestStatusEnrollment(serverAddress: string) {
    setError(null);
    try {
      if (!clientRuns()) {
        await ensureDesktopClientStarted();
      }
      const request = await api.requestStatusEnrollment(serverAddress);
      await refreshSnapshot();
      return request;
    } catch (err) {
      const message = formatPeerConnectionError(err, "peer-status");
      setError(message);
      throw shownFailure(new Error(message));
    }
  }

  async function removePeer(peerId: string, agentDid?: string) {
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.removePeer(peerId));
      if (agentDid && stores.selection.getState().agentDid === agentDid) {
        selectAgent(null);
      }
      return next;
    } catch (err) {
      const message = formatPeerConnectionError(err, "remove-peer");
      setError(message);
      throw shownFailure(new Error(message));
    }
  }

  async function renamePeer(peerId: string, label: string) {
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.renamePeer(peerId, label));
      return next;
    } catch (err) {
      const message = formatPeerConnectionError(err, "rename-peer");
      setError(message);
      throw shownFailure(new Error(message));
    }
  }

  return {
    fetchPeerStatus,
    requestStatusEnrollment,
    initLocalRuntime,
    removePeer,
    renamePeer,
  };
}
