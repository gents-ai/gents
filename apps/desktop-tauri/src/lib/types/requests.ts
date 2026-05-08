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
  allowedMcpServiceIds: string[];
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
