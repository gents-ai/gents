export type SavedPeer = {
  peerId: string;
  label: string;
  agentDid: string;
  addr: string;
  source?: string | null;
  graphql?: string | null;
};

export type BootstrapSummary = {
  defaultAgentHome: string;
  desktopHome: string;
  peerDirectoryPath: string;
  nodeDataDir: string;
  agentHomeExists: boolean;
  desktopHomeExists: boolean;
  peerDirectoryExists: boolean;
  savedPeers: SavedPeer[];
};

export type InitSummary = {
  status: string;
  source: string;
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
};

export type RuntimeView = {
  processState?: string | null;
  reconcilePhase?: string | null;
  lastReconcileResult?: string | null;
  lastReconcileError?: string | null;
  updatedAt?: string | null;
};

export type BehaviorView = {
  behaviorId: string;
  displayName: string;
  modelName?: string | null;
  enabled: boolean;
  isDefault: boolean;
};

export type ConversationSummary = {
  sessionId: string;
  title?: string | null;
  previewText?: string | null;
  status?: string | null;
  behaviorId?: string | null;
  latestRequestId?: string | null;
  createdAt?: string | null;
  updatedAt?: string | null;
  turnState?: string | null;
  messageCount: number;
  toolCallCount: number;
};

export type DeploymentView = {
  peerId: string;
  label: string;
  agentDid: string;
  addr: string;
  source?: string | null;
  graphql?: string | null;
  dialSucceeded: boolean;
  lastError?: string | null;
  defaultBehaviorId?: string | null;
  runtime?: RuntimeView | null;
  behaviors: BehaviorView[];
  conversations: ConversationSummary[];
};

export type RuntimeSnapshot = {
  localPeerId: string;
  listenAddresses: string[];
  p2pHealth: P2PHealth;
  bootstrapErrors: string[];
  lastMutationError?: string | null;
  focusedRequestId?: string | null;
  configuredPeerCount: number;
  dialedPeerCount: number;
  peerIssueCount: number;
  rowCount: number;
  approxSerializedBytes: number;
  deployments: DeploymentView[];
};

export type DesktopClientSnapshot = {
  bootstrap: BootstrapSummary;
  client?: RuntimeSnapshot | null;
};

export type MessageView = {
  messageKey: string;
  sequence?: number | null;
  role?: string | null;
  content?: string | null;
  displayRole?: string | null;
  displayContent?: string | null;
  reasoning?: string | null;
  hasToolCalls: boolean;
  hasToolResults: boolean;
  timestamp?: string | null;
};

export type ToolCallView = {
  toolCallKey: string;
  messageSequence?: number | null;
  toolName?: string | null;
  toolCallId?: string | null;
  args?: string | null;
  result?: string | null;
  status?: string | null;
  startedAt?: string | null;
  completedAt?: string | null;
};

export type ToolResultView = {
  toolName?: string | null;
  toolInput?: string | null;
  outputText?: string | null;
  truncated?: boolean | null;
  createdAt?: string | null;
};

export type ResponseView = {
  status?: string | null;
  content?: string | null;
  reasoning?: string | null;
  errorMessage?: string | null;
  tokenCount?: number | null;
  materializedMessageSequence?: number | null;
  materializedAt?: string | null;
  completedAt?: string | null;
};

export type DesktopSessionSnapshot = {
  sessionId: string;
  agentDid?: string | null;
  behaviorId?: string | null;
  title?: string | null;
  previewText?: string | null;
  status?: string | null;
  turnState?: string | null;
  latestRequestId?: string | null;
  latestResponse?: ResponseView | null;
  activeResponseOverlay?: ResponseView | null;
  messages: MessageView[];
  toolCalls: ToolCallView[];
  toolResults: ToolResultView[];
};

export type ChatSendResult = {
  sessionId: string;
  requestId: string;
  agentDid: string;
  behaviorId?: string | null;
};

const PLACEHOLDER_AGENT_DID_PREFIX = "did:defra-agent:default";

export function displayAgentIdentity(value?: string | null) {
  if (!value || value.startsWith(PLACEHOLDER_AGENT_DID_PREFIX)) {
    return null;
  }
  return value;
}

export function displayBehaviorLabel(value?: string | null) {
  if (
    !value ||
    value === "default" ||
    value.startsWith(PLACEHOLDER_AGENT_DID_PREFIX)
  ) {
    return null;
  }
  return value;
}

export function displayConversationTitle(value?: string | null) {
  const trimmed = value?.trim();
  return trimmed && trimmed.length > 0 ? trimmed : "untitled";
}

export function formatBytes(value: number) {
  if (value < 1024) {
    return `${value} B`;
  }
  if (value < 1024 * 1024) {
    return `${(value / 1024).toFixed(1)} KB`;
  }
  return `${(value / (1024 * 1024)).toFixed(1)} MB`;
}
