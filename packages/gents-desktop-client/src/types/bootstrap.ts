export type { BearerPairingResponse } from "../generated/BearerPairingResponse.js";
export type { BearerPairingView as BearerPairingResult } from "../generated/BearerPairingView.js";
export type { DesktopBootstrapSummary as BootstrapSummary } from "../generated/DesktopBootstrapSummary.js";
export type { P2PHealthView as P2PHealth } from "../generated/P2PHealthView.js";
export type { SavedPeerView as SavedPeer } from "../generated/SavedPeerView.js";
export type { BearerPairingRequest, PeerAddRequest } from "./requests.js";

export type InitSummary = {
  status: string;
  source: string;
  statusEndpoint?: string | null;
  agentHome: string;
  desktopHome: string;
  peerDirectory: string;
  label: string;
  agentName: string;
  agentDid: string;
  graphql: string;
  p2pTransport: string;
  p2pPeerId: string;
  p2pListenAddress: string;
  peerRecordId: string;
  nextSteps: string[];
};
