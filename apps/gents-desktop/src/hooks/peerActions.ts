import {
  formatPeerConnectionError,
  DEFAULT_NODE_NAME,
} from "@source-inc/gents-desktop-fleet";
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
  ensureDesktopClientStarted: () => Promise<DesktopClientSnapshot>;
  mutateSnapshot: <T>(operation: () => Promise<T>) => Promise<T>;
  refreshSnapshot: () => Promise<void>;
  reportFailure: (message: string) => void;
  selectNode: (nodeDid: string | null) => void;
};

export function createPeerActions({
  api,
  stores,
  ensureDesktopClientStarted,
  mutateSnapshot,
  refreshSnapshot,
  reportFailure,
  selectNode,
}: PeerActionParams) {
  const setStarting = (starting: boolean) =>
    clientStatus.setStarting(stores.client, starting);
  const clientRuns = () => Boolean(stores.client.getState().snapshot?.client);
  async function initLocalRuntime(label?: string | null) {
    const clientWasRunning = clientRuns();
    setStarting(true);
    try {
      if (clientWasRunning) {
        await mutateSnapshot(() => api.shutdownDesktopClient());
      }
      const summary = await api.initLocalStandardRuntime({
        label: label?.trim() || DEFAULT_NODE_NAME,
        dangerouslyOverwrite: false,
        reset: false,
      });
      await ensureDesktopClientStarted();
      selectNode(summary.nodeDid);
      return summary;
    } catch (err) {
      if (clientWasRunning) {
        try {
          await mutateSnapshot(() => api.startDesktopClient());
        } catch {}
      }
      throw new Error(formatPeerConnectionError(err, "local-runtime"));
    } finally {
      setStarting(false);
    }
  }

  async function reconnectNode(nodeDid: string | null): Promise<string | null> {
    if (!api.reconnectNode || !clientRuns()) return null;
    return api.reconnectNode(nodeDid);
  }

  async function requestRuntimeAccess(serverAddress: string) {
    try {
      if (!clientRuns()) {
        await ensureDesktopClientStarted();
      }
      const request = await api.requestRuntimeAccess(serverAddress);
      await refreshSnapshot();
      return request;
    } catch (err) {
      throw new Error(formatPeerConnectionError(err, "peer-status"));
    }
  }

  async function removeRuntime(nodeDid: string) {
    try {
      const next = await mutateSnapshot(() => api.removeRuntime(nodeDid));
      if (nodeDid && stores.selection.getState().nodeDid === nodeDid) {
        selectNode(null);
      }
      return next;
    } catch (err) {
      const message = formatPeerConnectionError(err, "remove-peer");
      reportFailure(message);
      throw shownFailure(new Error(message));
    }
  }

  function renameRuntime(nodeDid: string, label: string) {
    return mutateSnapshot(() => api.renameRuntime(nodeDid, label)).catch((err) => {
      throw new Error(formatPeerConnectionError(err, "rename-peer"));
    });
  }

  return {
    reconnectNode,
    requestRuntimeAccess,
    initLocalRuntime,
    removeRuntime,
    renameRuntime,
  };
}
