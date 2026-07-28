import type { BearerPairingRequest, PeerAddRequest } from "../types.js";
import { getDesktopApiAdapter } from "./adapter.js";
import type { DesktopApiAdapter } from "./types.js";

export function addPeer(request: PeerAddRequest, api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).addPeer(request);
}

export function pairBearer(
  request: BearerPairingRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).pairBearer(request);
}

export function removePeer(peerId: string, api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).removePeer(peerId);
}

export function renamePeer(
  peerId: string,
  label: string,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).renamePeer(peerId, label);
}

export function fetchPeerStatus(peerId: string, api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).fetchPeerStatus(peerId);
}

export function probePeerAddress(
  serverAddress: string,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).probePeerAddress(serverAddress);
}

export function repairP2P(api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).repairP2P();
}

export function fetchNetworkStatus(api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).fetchNetworkStatus();
}
