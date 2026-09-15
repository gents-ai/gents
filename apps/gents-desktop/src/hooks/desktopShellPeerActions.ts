import type { Dispatch, MutableRefObject, SetStateAction } from "react";

import { formatPeerConnectionError } from "@source-inc/gents-desktop-fleet";
import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";

type PeerActionParams = {
  api: DesktopApiAdapter;
  snapshot: DesktopClientSnapshot | null;
  /** Shared single-flight start used by autostart and peer actions. */
  ensureDesktopClientStarted: () => Promise<DesktopClientSnapshot | null>;
  mutateSnapshot: <T>(operation: () => Promise<T>) => Promise<T>;
  refreshSnapshot: () => Promise<void>;
  setAddingPeer: Dispatch<SetStateAction<boolean>>;
  setError: Dispatch<SetStateAction<string | null>>;
  setRepairingP2P: Dispatch<SetStateAction<boolean>>;
  selectedAgentDidRef: MutableRefObject<string | null>;
  selectAgent: (agentDid: string | null) => void;
  setStarting: Dispatch<SetStateAction<boolean>>;
};

export function createDesktopShellPeerActions({
  api,
  snapshot,
  ensureDesktopClientStarted,
  mutateSnapshot,
  refreshSnapshot,
  setAddingPeer,
  setError,
  setRepairingP2P,
  selectedAgentDidRef,
  selectAgent,
  setStarting,
}: PeerActionParams) {
  async function onInitLocalRuntime(label?: string | null) {
    const clientWasRunning = Boolean(snapshot?.client);
    setAddingPeer(true);
    setStarting(true);
    setError(null);
    try {
      if (snapshot?.client) {
        await mutateSnapshot(() => api.shutdownDesktopClient());
      }
      const summary = await api.initLocalStandardRuntime({
        label: label?.trim() || "Local Agent",
        dangerouslyOverwrite: false,
        reset: false,
      });
      // Init durably writes the local-standard peer entry. Restarting the
      // client is the only supported way to hydrate that trusted local route.
      const next = await ensureDesktopClientStarted();
      if (!next) {
        throw new Error("desktop client failed to start after local runtime init");
      }
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
      throw new Error(message);
    } finally {
      setStarting(false);
      setAddingPeer(false);
    }
  }

  async function onFetchPeerStatus(peerId: string) {
    setError(null);
    try {
      return await api.fetchPeerStatus(peerId);
    } catch (err) {
      const message = formatPeerConnectionError(err, "peer-status");
      setError(message);
      throw new Error(message);
    }
  }

  async function onRequestStatusEnrollment(serverAddress: string) {
    setError(null);
    try {
      if (!snapshot?.client) {
        const started = await ensureDesktopClientStarted();
        if (!started) {
          throw new Error("desktop client failed to start before enrollment");
        }
      }
      const request = await api.requestStatusEnrollment(serverAddress);
      await refreshSnapshot();
      return request;
    } catch (err) {
      const message = formatPeerConnectionError(err, "peer-status");
      setError(message);
      throw new Error(message);
    }
  }

  async function onRemovePeer(peerId: string, agentDid?: string) {
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.removePeer(peerId));
      if (agentDid && selectedAgentDidRef.current === agentDid) {
        selectAgent(null);
      }
      return next;
    } catch (err) {
      const message = formatPeerConnectionError(err, "remove-peer");
      setError(message);
      throw new Error(message);
    }
  }

  async function onRenamePeer(peerId: string, label: string) {
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.renamePeer(peerId, label));
      return next;
    } catch (err) {
      const message = formatPeerConnectionError(err, "rename-peer");
      setError(message);
      throw new Error(message);
    }
  }

  async function onRepairP2P() {
    setRepairingP2P(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.repairP2P());
      return next;
    } catch (err) {
      const message = formatPeerConnectionError(err, "repair-p2p");
      setError(message);
      throw new Error(message);
    } finally {
      setRepairingP2P(false);
    }
  }

  return {
    onFetchPeerStatus,
    onRequestStatusEnrollment,
    onInitLocalRuntime,
    onRemovePeer,
    onRenamePeer,
    onRepairP2P,
  };
}
