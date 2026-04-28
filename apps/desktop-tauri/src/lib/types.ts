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
  lastOkAt?: string | null;
  lastFailureAt?: string | null;
};

export type RuntimeView = {
  processState?: string | null;
  reconcilePhase?: string | null;
  lastReconcileResult?: string | null;
  lastReconcileError?: string | null;
  updatedAt?: string | null;
};

export type AgentPrincipalView = {
  agentDid: string;
  displayName?: string | null;
  defaultBehaviorId?: string | null;
  enabled?: boolean | null;
  createdAt?: string | null;
  createdBy?: string | null;
};

export type BehaviorView = {
  behaviorId: string;
  displayName: string;
  systemPrompt?: string | null;
  backendId?: string | null;
  modelName?: string | null;
  toolSelectionId?: string | null;
  inferenceProfileId?: string | null;
  compactionStrategy?: string | null;
  compactionThreshold?: number | null;
  enabled: boolean;
  isDefault: boolean;
};

export type InferenceBackendView = {
  backendId: string;
  name?: string | null;
  providerKind?: string | null;
  endpoint?: string | null;
  apiKeyConfigured: boolean;
  apiKeyEnvVar?: string | null;
  maxConcurrent?: number | null;
  maxQueueDepth?: number | null;
  enabled?: boolean | null;
  models: string[];
  probeStatus?: string | null;
};

export type InferenceProfileView = {
  profileId: string;
  displayName?: string | null;
  contextWindow?: number | null;
  maxOutputTokens?: number | null;
  maxTurns?: number | null;
  temperature?: number | null;
  streamBatchMs?: number | null;
  deadlineDurationSecs?: number | null;
};

export type ToolSelectionView = {
  selectionId: string;
  agentDid?: string | null;
  displayName?: string | null;
  enableFileTools?: boolean | null;
  fileToolsMode?: string | null;
  fileToolRoot?: string | null;
  enableBash?: boolean | null;
  bashMode?: string | null;
  cliToolNames: string[];
  enableMetaTools?: boolean | null;
  delegateTo: string[];
};

export type ToolServiceRegistryView = {
  serviceId: string;
  displayName?: string | null;
  description?: string | null;
  hostname?: string | null;
  tailscaleIp?: string | null;
  lanIp?: string | null;
  mcpPort?: number | null;
  mcpPath?: string | null;
  status?: string | null;
  version?: string | null;
  updatedAt?: string | null;
};

export type TaskView = {
  taskId: string;
  name?: string | null;
  description?: string | null;
  behaviorId?: string | null;
  promptTemplate?: string | null;
  enabled?: boolean | null;
  outputSchemaRef?: string | null;
  recentRuns: TaskRecentRunsView;
  runHistory: TaskRunSummaryView[];
};

export type TaskRecentRunsView = {
  totalFires: number;
  lastAttemptAt?: string | null;
  lastStatus?: string | null;
  lastError?: string | null;
  scheduleCount: number;
  eventTriggerCount: number;
};

export type TaskRunSummaryView = {
  requestId: string;
  sessionId?: string | null;
  behaviorId?: string | null;
  status?: string | null;
  lifecycleState?: string | null;
  executionOrigin?: string | null;
  causedByTriggerId?: string | null;
  causedByTriggerKind?: string | null;
  createdAt?: string | null;
};

export type ScheduleView = {
  scheduleId: string;
  taskId?: string | null;
  intervalSecs?: number | null;
  enabled?: boolean | null;
  concurrency?: string | null;
  nextRunAt?: string | null;
  lastAttemptAt?: string | null;
  lastStatus?: string | null;
  lastError?: string | null;
  fireCount?: number | null;
};

export type EventTriggerView = {
  triggerId: string;
  taskId?: string | null;
  sourceCollection?: string | null;
  eventKind?: string | null;
  filter?: string | null;
  enabled?: boolean | null;
  concurrency?: string | null;
  lastAttemptAt?: string | null;
  lastFiredSourceDocId?: string | null;
  lastStatus?: string | null;
  lastError?: string | null;
  fireCount?: number | null;
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
  agentPrincipal: AgentPrincipalView;
  runtime?: RuntimeView | null;
  behaviors: BehaviorView[];
  inferenceBackends: InferenceBackendView[];
  inferenceProfiles: InferenceProfileView[];
  toolSelections: ToolSelectionView[];
  toolServiceRegistries: ToolServiceRegistryView[];
  tasks: TaskView[];
  schedules: ScheduleView[];
  eventTriggers: EventTriggerView[];
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

export type ToolDetailFieldView = {
  key: string;
  value: string;
};

export type ToolDetailValueView = {
  rawText: string;
  fields: ToolDetailFieldView[];
};

export type RenderedToolCallView = {
  itemKey: string;
  toolName: string;
  status?: string | null;
  statusKind: string;
  args?: ToolDetailValueView | null;
  result?: ToolDetailValueView | null;
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

export type PendingTurnView = {
  requestId: string;
  content: string;
  lifecycleState?: string | null;
  createdAt?: string | null;
};

export type RenderedTimelineItem =
  | {
      kind: "userMessage";
      itemKey: string;
      sequence?: number | null;
      content: string;
    }
  | {
      kind: "assistantMessage";
      itemKey: string;
      sequence?: number | null;
      content?: string | null;
      reasoning?: string | null;
    }
  | {
      kind: "toolGroup";
      itemKey: string;
      messageSequence?: number | null;
      tools: RenderedToolCallView[];
    }
  | {
      kind: "pendingUserTurn";
      itemKey: string;
      requestId: string;
      content: string;
      lifecycleState?: string | null;
      createdAt?: string | null;
    }
  | {
      kind: "liveAssistant";
      itemKey: string;
      content?: string | null;
      reasoning?: string | null;
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
  pendingTurn?: PendingTurnView | null;
  timelineItems: RenderedTimelineItem[];
};

export type ChatSendResult = {
  sessionId: string;
  requestId: string;
  agentDid: string;
  behaviorId?: string | null;
};

export type AgentConfigSaveRequest = {
  agentDid: string;
  displayName: string;
  defaultBehaviorId: string;
  enabled?: boolean | null;
};

export type BehaviorSaveRequest = {
  agentDid: string;
  behaviorId: string;
  displayName: string;
  systemPrompt: string;
  backendId?: string | null;
  toolSelectionId?: string | null;
  inferenceProfileId?: string | null;
  compactionStrategy?: string | null;
  compactionThreshold?: number | null;
  enabled?: boolean | null;
};

export type BackendSaveRequest = {
  backendId: string;
  name: string;
  providerKind: string;
  endpoint: string;
  apiKey?: string | null;
  apiKeyEnvVar?: string | null;
  clearApiKey?: boolean | null;
  models: string[];
  maxConcurrent?: number | null;
  maxQueueDepth?: number | null;
  enabled?: boolean | null;
};

export type InferenceProfileSaveRequest = {
  profileId: string;
  displayName: string;
  contextWindow?: number | null;
  maxOutputTokens?: number | null;
  maxTurns?: number | null;
  temperature?: number | null;
  streamBatchMs?: number | null;
  deadlineDurationSecs?: number | null;
};

export type ToolSelectionSaveRequest = {
  agentDid: string;
  selectionId: string;
  displayName: string;
  enableFileTools?: boolean | null;
  fileToolsMode?: string | null;
  fileToolRoot?: string | null;
  enableBash?: boolean | null;
  bashMode?: string | null;
  cliToolNames: string[];
  enableMetaTools?: boolean | null;
  delegateTo: string[];
};

export type ToolServiceSaveRequest = {
  serviceId: string;
  displayName: string;
  description?: string | null;
  hostname?: string | null;
  tailscaleIp?: string | null;
  lanIp?: string | null;
  mcpPort?: number | null;
  mcpPath?: string | null;
  status?: string | null;
};

export type ToolServiceTestRequest = {
  serviceId: string;
  hostname?: string | null;
  tailscaleIp?: string | null;
  lanIp?: string | null;
  mcpPort?: number | null;
  mcpPath?: string | null;
};

export type ToolServiceToolView = {
  name: string;
  description?: string | null;
};

export type ToolServiceTestResult = {
  serviceId: string;
  endpoint: string;
  status: string;
  toolCount: number;
  tools: ToolServiceToolView[];
  error?: string | null;
};

export type TaskSaveRequest = {
  taskId: string;
  name: string;
  description?: string | null;
  behaviorId: string;
  promptTemplate: string;
  enabled?: boolean | null;
  outputSchemaRef?: string | null;
};

export type TaskRunRequest = {
  taskId: string;
  args?: unknown;
};

export type ScheduleSaveRequest = {
  scheduleId: string;
  taskId: string;
  intervalSecs?: number | null;
  enabled?: boolean | null;
  concurrency?: string | null;
};

export type ScheduleRunRequest = {
  scheduleId: string;
};

export type EventTriggerSaveRequest = {
  triggerId: string;
  taskId: string;
  sourceCollection: string;
  eventKind: string;
  filter?: string | null;
  enabled?: boolean | null;
  concurrency?: string | null;
};

export type TaskRunResult = {
  requestDocId: string;
  requestId: string;
  sessionId: string;
  agentDid: string;
  behaviorId: string;
  status?: string | null;
  lifecycleState?: string | null;
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
