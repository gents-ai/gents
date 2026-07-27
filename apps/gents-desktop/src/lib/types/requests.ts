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
  skillRefs: string[];
  skillExcludes: string[];
};

export type BackendSaveRequest = {
  backendId: string;
  name: string;
  providerKind: string;
  openaiWireApi?: string | null;
  endpoint: string;
  apiKey?: string | null;
  apiKeyEnvVar?: string | null;
  clearApiKey?: boolean | null;
  models: string[];
  maxConcurrent?: number | null;
  maxQueueDepth?: number | null;
  enabled?: boolean | null;
};

export type InferenceProbeResult = {
  reachable: boolean;
  models: string[];
};

export type CodexLoginResult = {
  docId: string;
  credentialId: string;
  agentDid: string;
  provider: string;
  accountId?: string | null;
  chatgptPlanType?: string | null;
  isFedramp: boolean;
  accessTokenExpiresAt: string;
  enabled: boolean;
};

export type InferenceProfileSaveRequest = {
  profileId: string;
  displayName: string;
  contextWindow?: number | null;
  maxOutputTokens?: number | null;
  maxTurns?: number | null;
  temperature?: number | null;
  streamBatchMs?: number | null;
  streamLivenessTimeoutSecs?: number | null;
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
  // Editable query allowlist: omit to preserve the stored value (the bridge
  // preserves-on-absent); send [] to clear. Mirrors the Rust
  // `Option<Vec<String>>` preserve-on-None semantics.
  defraQueryCollections?: string[] | null;
  subagentDefaultAwaitMode?: string | null;
  orchestrationEnabled?: boolean | null;
  // `writeTools` and `toolPolicyVersion` are deliberately NOT part of the save
  // request — preserve-only (apply-managed / backfill-owned). See the Rust
  // `ToolSelectionSaveRequest` for the rationale.
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

export type SkillSaveRequest = {
  skillId: string;
  agentDid: string;
  scope: string;
  name: string;
  description?: string | null;
  instructions: string;
  toolRefs: string[];
  displayName?: string | null;
  enabled?: boolean | null;
};

export type SkillDeleteRequest = {
  skillId: string;
  agentDid: string;
};

export type TaskDeleteRequest = {
  taskId: string;
  agentDid: string;
};

export type ScheduleDeleteRequest = {
  scheduleId: string;
  agentDid: string;
};

export type EventTriggerDeleteRequest = {
  triggerId: string;
  agentDid: string;
};

export type BackendDeleteRequest = {
  backendId: string;
  agentDid: string;
};

export type InferenceProfileDeleteRequest = {
  profileId: string;
  agentDid: string;
};

export type ToolSelectionDeleteRequest = {
  selectionId: string;
  agentDid: string;
};

export type ToolServiceDeleteRequest = {
  serviceId: string;
  agentDid: string;
};

export type BehaviorDeleteRequest = {
  behaviorId: string;
  agentDid: string;
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
