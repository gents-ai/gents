import type {
  InferenceBackendView,
  InferenceProfileView,
  ScheduleView,
  TaskRunResult,
  TaskView,
  ToolSelectionView,
  ToolServiceRegistryView,
} from "../../src/lib/types";

export const backend: InferenceBackendView = {
  backendId: "default-backend",
  name: "Default Backend",
  providerKind: "openai",
  endpoint: "http://127.0.0.1:8000/v1",
  apiKeyConfigured: false,
  maxConcurrent: 2,
  maxQueueDepth: 20,
  enabled: true,
  models: ["baa-ai/model"],
};

export const profile: InferenceProfileView = {
  profileId: "default-profile",
  displayName: "Default Profile",
  contextWindow: 131072,
  maxOutputTokens: 32768,
  temperature: 0,
};

export const toolSelection: ToolSelectionView = {
  selectionId: "default-tools",
  agentDid: "did:key:z6MkAgent",
  displayName: "Default Tools",
  enableFileTools: true,
  fileToolsMode: "ReadOnly",
  fileToolRoot: "/tmp/work",
  enableBash: true,
  bashMode: "ReadOnly",
  cliToolNames: ["grep"],
  enableMetaTools: true,
  allowedMcpServiceIds: ["mcp-local"],
  delegateTo: [],
  enableDefraQuery: true,
  defraQueryCollections: ["AgentRequest"],
  // Each entry is a JSON-serialized WriteToolDecl, as the real bridge emits.
  writeTools: [
    '{"tool_name":"upsert_note","collection":"Note","description":"","fields":[]}',
  ],
  toolPolicyVersion: "tool-policy/v1",
};

export const toolService: ToolServiceRegistryView = {
  serviceId: "mcp-local",
  displayName: "Local MCP",
  description: "Local tools",
  hostname: "localhost",
  mcpPort: 7331,
  mcpPath: "/mcp",
  status: "online",
};

export const task: TaskView = {
  taskId: "task-a",
  name: "Task A",
  description: "Runs task A",
  behaviorId: "default",
  promptTemplate: "Run task A",
  enabled: true,
  outputSchemaRef: null,
  recentRuns: {
    totalFires: 0,
    scheduleCount: 0,
    eventTriggerCount: 0,
  },
  runHistory: [],
};

export const schedule: ScheduleView = {
  scheduleId: "timer-a",
  taskId: "task-a",
  intervalSecs: 60,
  enabled: true,
  concurrency: "serial",
  fireCount: 0,
};

export const runResult: TaskRunResult = {
  requestDocId: "bae-run",
  requestId: "run-1",
  sessionId: "session-1",
  agentDid: "did:key:z6MkAgent",
  behaviorId: "default",
  status: "submitted",
  lifecycleState: "queued",
};
