import { formatPeerConnectionError } from "@source-inc/gents-desktop-fleet";
import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";
import { shownFailure } from "./actionFailure";
import type { ShellStores } from "./shellProjection";
import { clientStatus } from "./clientStore";

type PeerActionParams = {
  api: DesktopApiAdapter;
  stores: ShellStores;
  /** Shared single-flight start used by autostart and peer actions. */
  ensureDesktopClientStarted: () => Promise<DesktopClientSnapshot>;
  mutateSnapshot: <T>(operation: () => Promise<T>) => Promise<T>;
  refreshSnapshot: () => Promise<void>;
  /** shows a failed action to the person, once */
  reportFailure: (message: string) => void;
  selectAgent: (agentDid: string | null) => void;
};

export function createPeerActions({
  api,
  stores,
  ensureDesktopClientStarted,
  mutateSnapshot,
  refreshSnapshot,
  reportFailure,
  selectAgent,
}: PeerActionParams) {
  const setStarting = (starting: boolean) =>
    clientStatus.setStarting(stores.client, starting);
  /** whether the client runs, as last read */
  const clientRuns = () => Boolean(stores.client.getState().snapshot?.client);
  async function initLocalRuntime(label?: string | null) {
    const clientWasRunning = clientRuns();
    setStarting(true);
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
      throw new Error(formatPeerConnectionError(err, "local-runtime"));
    } finally {
      setStarting(false);
    }
  }

  async function fetchPeerStatus(peerId: string) {
    try {
      return await api.fetchPeerStatus(peerId);
    } catch (err) {
      throw new Error(formatPeerConnectionError(err, "peer-status"));
    }
  }

  async function requestStatusEnrollment(serverAddress: string) {
    try {
      if (!clientRuns()) {
        await ensureDesktopClientStarted();
      }
      const request = await api.requestStatusEnrollment(serverAddress);
      await refreshSnapshot();
      return request;
    } catch (err) {
      throw new Error(formatPeerConnectionError(err, "peer-status"));
    }
  }

  async function removePeer(peerId: string, agentDid?: string) {
    try {
      const next = await mutateSnapshot(() => api.removePeer(peerId));
      if (agentDid && stores.selection.getState().agentDid === agentDid) {
        selectAgent(null);
      }
      return next;
    } catch (err) {
      const message = formatPeerConnectionError(err, "remove-peer");
      reportFailure(message);
      throw shownFailure(new Error(message));
    }
  }

  function renamePeer(peerId: string, label: string) {
    return mutateSnapshot(() => api.renamePeer(peerId, label)).catch((err) => {
      throw new Error(formatPeerConnectionError(err, "rename-peer"));
    });
  }

  return {
    /**
     * Asks a peer for its status, with nothing stored. A failure is thrown
     * in the peer's words, for the screen to show.
     */
    fetchPeerStatus,
    /**
     * Asks the server at the address to enroll this desktop, starting the
     * client first if it is not running, then reads the client again. A
     * failure is thrown in the peer's words, for the screen to show.
     */
    requestStatusEnrollment,
    /**
     * Provisions the local agent and starts the client on its route, then
     * selects it. A running client is stopped first and, if provisioning
     * fails, started again. A failure is not reported here: setup shows it in
     * its step log, with a retry.
     */
    initLocalRuntime,
    /**
     * Removes a paired peer and reads the client again; when the removed node
     * was selected, nothing is selected after. A failure is reported once,
     * then rethrown.
     */
    removePeer,
    /**
     * Saves a peer's label on this desktop and reads the client again. A
     * failure is not reported here: the rename dialog shows it inline and
     * stays open.
     */
    renamePeer,
  };
}
