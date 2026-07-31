import type { Dispatch, SetStateAction } from "react";

import { formatPeerConnectionError } from "@source-inc/gents-desktop-fleet";
import type {
  BearerPairingRequest,
  DesktopApiAdapter,
  DesktopClientSnapshot,
  PeerAddRequest,
} from "@source-inc/gents-desktop-client";

type PeerActionParams = {
  api: DesktopApiAdapter;
  snapshot: DesktopClientSnapshot | null;
  /** Shared single-flight start used by autostart and peer actions. */
  ensureDesktopClientStarted: () => Promise<DesktopClientSnapshot | null>;
  setAddingPeer: Dispatch<SetStateAction<boolean>>;
  setError: Dispatch<SetStateAction<string | null>>;
  setRepairingP2P: Dispatch<SetStateAction<boolean>>;
  setSelectedAgentDid: Dispatch<SetStateAction<string | null>>;
  setSnapshot: Dispatch<SetStateAction<DesktopClientSnapshot | null>>;
  setStarting: Dispatch<SetStateAction<boolean>>;
};

export function createDesktopShellPeerActions({
  api,
  snapshot,
  ensureDesktopClientStarted,
  setAddingPeer,
  setError,
  setRepairingP2P,
  setSelectedAgentDid,
  setSnapshot,
  setStarting,
}: PeerActionParams) {
  async function onInitLocalRuntime(label?: string | null) {
    setAddingPeer(true);
    setStarting(true);
    setError(null);
    try {
      const summary = await api.initLocalStandardRuntime({
        label: label?.trim() || "Local Agent",
        dangerouslyOverwrite: false,
        reset: false,
      });
      const peerRequest: PeerAddRequest = {
        label: summary.label,
        agentDid: summary.agentDid,
        addr: summary.p2pListenAddress,
        graphql: summary.graphql,
      };
      // Init writes the peer directory entry; a fresh start bootstraps it.
      // A live client needs an explicit addPeer for the new record.
      const next = snapshot?.client
        ? await api.addPeer(peerRequest)
        : await ensureDesktopClientStarted();
      if (!next) {
        throw new Error("desktop client failed to start after local runtime init");
      }
      setSnapshot(next);
      setSelectedAgentDid(summary.agentDid);
      return summary;
    } catch (err) {
      const message = formatPeerConnectionError(err, "local-runtime");
      setError(message);
      throw new Error(message);
    } finally {
      setStarting(false);
      setAddingPeer(false);
    }
  }

  async function onAddPeer(request: PeerAddRequest) {
    setAddingPeer(true);
    setError(null);
    try {
      if (!snapshot?.client) {
        const started = await ensureDesktopClientStarted();
        if (!started) {
          throw new Error("desktop client failed to start before adding peer");
        }
      }
      const next = await api.addPeer(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
      return next;
    } catch (err) {
      const message = formatPeerConnectionError(err, "add-peer");
      setError(message);
      throw new Error(message);
    } finally {
      setStarting(false);
      setAddingPeer(false);
    }
  }

  async function onPairBearer(request: BearerPairingRequest) {
    setAddingPeer(true);
    setError(null);
    try {
      if (!snapshot?.client) {
        const started = await ensureDesktopClientStarted();
        if (!started) {
          throw new Error("desktop client failed to start before pairing");
        }
      }
      const response = await api.pairBearer(request);
      setSnapshot({
        bootstrap: response.bootstrap,
        client: response.client,
      });
      setSelectedAgentDid(response.pairing.issuerDid);
      return response;
    } catch (err) {
      const message = formatPeerConnectionError(err, "add-peer");
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

  async function onProbePeerAddress(serverAddress: string) {
    setError(null);
    try {
      return await api.probePeerAddress(serverAddress);
    } catch (err) {
      const message = formatPeerConnectionError(err, "peer-status");
      setError(message);
      throw new Error(message);
    }
  }

  async function onRemovePeer(peerId: string) {
    setError(null);
    try {
      const next = await api.removePeer(peerId);
      setSnapshot(next);
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
      const next = await api.renamePeer(peerId, label);
      setSnapshot(next);
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
      const next = await api.repairP2P();
      setSnapshot(next);
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
    onAddPeer,
    onFetchPeerStatus,
    onProbePeerAddress,
    onInitLocalRuntime,
    onPairBearer,
    onRemovePeer,
    onRenamePeer,
    onRepairP2P,
  };
}
