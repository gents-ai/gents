import type { BootstrapSummary, P2PHealth } from "./bootstrap";

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
  skillRefs: string[];
  skillExcludes: string[];
};

export type InferenceBackendView = {
  backendId: string;
  name?: string | null;
  providerKind?: string | null;
  openaiWireApi?: string | null;
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
  streamLivenessTimeoutSecs?: number | null;
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
  commandExecutionPolicy?: string | null;
  commandAllowedArgvPrefixes: string[];
  commandForbiddenArgvPrefixes: string[];
  commandNetworkMode?: string | null;
  cliToolNames: string[];
  enableMetaTools?: boolean | null;
  allowedMcpServiceIds: string[];
  delegateTo: string[];
  backgroundableToolNames: string[];
  subagentTargets: string[];
  subagentSpawnEnabled?: boolean | null;
  subagentSteeringEnabled?: boolean | null;
  subagentBackgroundEnabled?: boolean | null;
  crossDeploymentSpawnTimeoutSeconds?: number | null;
  enableMemory?: boolean | null;
  enableSessionHistoryTool?: boolean | null;
  enableContextBudget?: boolean | null;
  enableDefraQuery?: boolean | null;
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

export type SkillView = {
  skillId: string;
  agentDid?: string | null;
  scope?: string | null;
  name?: string | null;
  description?: string | null;
  instructions?: string | null;
  toolRefs: string[];
  displayName?: string | null;
  enabled?: boolean | null;
  createdAt?: string | null;
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
  taskId?: string | null;
  taskName?: string | null;
  triggerId?: string | null;
  triggerKind?: string | null;
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
  skills: SkillView[];
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
