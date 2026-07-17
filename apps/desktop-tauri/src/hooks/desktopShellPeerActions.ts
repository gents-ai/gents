import type { Dispatch, SetStateAction } from "react";

import {
  addPeer,
  fetchPeerStatus,
  initLocalStandardRuntime,
  removePeer,
  renamePeer,
  repairP2P,
  startDesktopClient,
} from "../lib/desktop-api";
import { formatPeerConnectionError } from "../lib/peerConnectionErrors";
import type { DesktopClientSnapshot, PeerAddRequest } from "../lib/types";

type PeerActionParams = {
  snapshot: DesktopClientSnapshot | null;
  setAddingPeer: Dispatch<SetStateAction<boolean>>;
  setError: Dispatch<SetStateAction<string | null>>;
  setRepairingP2P: Dispatch<SetStateAction<boolean>>;
  setSelectedAgentDid: Dispatch<SetStateAction<string | null>>;
  setSnapshot: Dispatch<SetStateAction<DesktopClientSnapshot | null>>;
  setStarting: Dispatch<SetStateAction<boolean>>;
};

export function createDesktopShellPeerActions({
  snapshot,
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
      const summary = await initLocalStandardRuntime({
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
      const next = snapshot?.client
        ? await addPeer(peerRequest)
        : await startDesktopClient();
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
        setStarting(true);
        const started = await startDesktopClient();
        setSnapshot(started);
      }
      const next = await addPeer(request);
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

  async function onFetchPeerStatus(serverAddress: string) {
    setError(null);
    try {
      return await fetchPeerStatus(serverAddress);
    } catch (err) {
      const message = formatPeerConnectionError(err, "peer-status");
      setError(message);
      throw new Error(message);
    }
  }

  async function onRemovePeer(peerId: string) {
    setError(null);
    try {
      const next = await removePeer(peerId);
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
      const next = await renamePeer(peerId, label);
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
      const next = await repairP2P();
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
    onInitLocalRuntime,
    onRemovePeer,
    onRenamePeer,
    onRepairP2P,
  };
}
