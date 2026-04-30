import type { Dispatch, SetStateAction } from "react";

import {
  addPeer,
  fetchPeerStatus,
  repairP2P,
  startDesktopClient,
} from "../lib/desktop-api";
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
      setError(String(err));
      throw err;
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
      setError(String(err));
      throw err;
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
      setError(String(err));
      throw err;
    } finally {
      setRepairingP2P(false);
    }
  }

  return {
    onAddPeer,
    onFetchPeerStatus,
    onRepairP2P,
  };
}
