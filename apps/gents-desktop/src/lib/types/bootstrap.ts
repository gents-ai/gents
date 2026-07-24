export type SavedPeer = {
  peerId: string;
  label: string;
  agentDid: string;
  addr: string;
  source?: string | null;
  graphql?: string | null;
};

export type PeerAddRequest = {
  label: string;
  agentDid: string;
  addr: string;
  graphql?: string | null;
};

export type BearerPairingRequest = {
  token: string;
  label?: string | null;
};

export type BearerPairingResult = {
  peerId: string;
  label: string;
  addr: string;
  issuerDid: string;
  claimantDid: string;
  networkId: string;
  template: string;
  connected: boolean;
  claimSubmitted: boolean;
  endpointPublished: boolean;
  replicationConfigured: boolean;
  membershipObserved: boolean;
  bidirectionalReplicationObserved: boolean;
};

export type BearerPairingResponse = {
  bootstrap: BootstrapSummary;
  client: import("./deployment").RuntimeSnapshot | null;
  pairing: BearerPairingResult;
};

export type BootstrapSummary = {
  defaultAgentHome: string;
  initAgentName?: string | null;
  initAgentDid?: string | null;
  initToolCeiling?: string | null;
  initToolRoot?: string | null;
  desktopHome: string;
  peerDirectoryPath: string;
  nodeDataDir: string;
  logFilePath: string;
  agentHomeExists: boolean;
  desktopHomeExists: boolean;
  peerDirectoryExists: boolean;
  savedPeers: SavedPeer[];
};

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

export type P2PHealth = {
  status: string;
  connectedPeerCount: number;
  replicatorCount: number;
  consecutiveFailures: number;
  lastError?: string | null;
  lastOkAt?: string | null;
  lastFailureAt?: string | null;
};
