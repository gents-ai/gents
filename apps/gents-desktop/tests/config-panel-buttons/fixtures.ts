import type {
  EventSource,
  InferenceBackendView,
  Schedule,
  TaskView,
  ToolServiceRegistry,
  Tools,
  TriggerView,
} from "@source-inc/gents-desktop-client";

export const backend: InferenceBackendView = {
  backendId: "default-backend",
  name: "Default Backend",
  providerKind: "OpenAiCompatible",
  endpoint: "http://127.0.0.1:8000/v1",
  apiKeyConfigured: false,
  maxConcurrent: 2,
  maxQueueDepth: 20,
  enabled: true,
  models: ["baa-ai/model"],
};

export const task: TaskView = {
  taskId: "task-a",
  name: "Task A",
  description: "Runs task A",
  behaviorId: "default",
  promptTemplate: "Run task A",
  goalObjectiveTemplate: null,
  goalTokenBudget: null,
  hooks: [],
  enabled: true,
  outputSchemaRef: null,
  tags: [],
  recentRuns: {
    totalFires: 0,
    lastAttemptAt: null,
    lastStatus: null,
    lastError: null,
    scheduleCount: 0,
    eventCount: 0,
  },
  runHistory: [],
};

export const schedule: Schedule = {
  agent_did: "did:key:z6MkAgent",
  schedule_id: "timer-a",
  display_name: "Timer A",
  cadence: { kind: "interval", interval_secs: 60 },
  tags: [],
};

export const eventSource: EventSource = {
  agent_did: "did:key:z6MkAgent",
  event_source_id: "source-a",
  display_name: "Source A",
  source_collection: "AgentRequest",
  event_kind: "created",
  filter: null,
  correlation_field: null,
  group: null,
  workspace_authority: null,
  tags: [],
};

export const triggerView: TriggerView = {
  config: {
    agent_did: "did:key:z6MkAgent",
    trigger_id: "trigger-a",
    display_name: "Trigger A",
    description: null,
    task_id: "task-a",
    source: { kind: "schedule", schedule_id: "timer-a" },
    enabled: true,
    concurrency: "serial",
    tags: [],
  },
  nextRunAt: null,
  lastAttemptAt: null,
  lastFiredSourceDocId: null,
  lastStatus: null,
  lastError: null,
  fireCount: 0,
};

export const tools: Tools = {
  tools_id: "default-tools",
  agent_did: "did:key:z6MkAgent",
};

export const toolService: ToolServiceRegistry = {
  service_id: "mcp-local",
  agent_did: "did:key:z6MkAgent",
  display_name: "Local MCP",
  hostname: "localhost",
  mcp_port: 7331,
  mcp_path: "/mcp",
};
