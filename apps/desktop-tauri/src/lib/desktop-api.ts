import { invoke } from "@tauri-apps/api/core";

import type {
  AgentConfigSaveRequest,
  BackendSaveRequest,
  BehaviorSaveRequest,
  ChatSendResult,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  EventTriggerSaveRequest,
  InferenceProfileSaveRequest,
  InitSummary,
  PeerAddRequest,
  ScheduleRunRequest,
  ScheduleSaveRequest,
  TaskRunRequest,
  TaskRunResult,
  TaskSaveRequest,
  ToolSelectionSaveRequest,
  ToolServiceSaveRequest,
  ToolServiceTestRequest,
  ToolServiceTestResult,
} from "./types";

export type DesktopApiAdapter = {
  fetchDesktopSnapshot: () => Promise<DesktopClientSnapshot>;
  initLocalStandardRuntime: (request: {
    label: string;
    dangerouslyOverwrite: boolean;
    reset: boolean;
  }) => Promise<InitSummary>;
  startDesktopClient: () => Promise<DesktopClientSnapshot>;
  shutdownDesktopClient: () => Promise<DesktopClientSnapshot>;
  addPeer: (request: PeerAddRequest) => Promise<DesktopClientSnapshot>;
  repairP2P: () => Promise<DesktopClientSnapshot>;
  fetchSessionSnapshot: (
    sessionId: string,
    requestId?: string | null,
  ) => Promise<DesktopSessionSnapshot | null>;
  sendChatMessage: (request: {
    agentDid: string;
    behaviorId?: string | null;
    sessionId?: string | null;
    content: string;
  }) => Promise<ChatSendResult>;
  renameConversation: (request: {
    sessionId: string;
    title: string;
  }) => Promise<void>;
  saveAgentConfig: (
    request: AgentConfigSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveBehaviorConfig: (
    request: BehaviorSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveBackendConfig: (
    request: BackendSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveInferenceProfileConfig: (
    request: InferenceProfileSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveToolSelectionConfig: (
    request: ToolSelectionSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveToolServiceConfig: (
    request: ToolServiceSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  testToolService: (
    request: ToolServiceTestRequest,
  ) => Promise<ToolServiceTestResult>;
  saveTaskConfig: (request: TaskSaveRequest) => Promise<DesktopClientSnapshot>;
  saveScheduleConfig: (
    request: ScheduleSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  runSchedule: (request: ScheduleRunRequest) => Promise<TaskRunResult>;
  saveEventTriggerConfig: (
    request: EventTriggerSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  runTask: (request: TaskRunRequest) => Promise<TaskRunResult>;
};

const defaultDesktopApiAdapter: DesktopApiAdapter = {
  fetchDesktopSnapshot() {
    return invoke<DesktopClientSnapshot>("desktop_client_snapshot");
  },
  initLocalStandardRuntime(request) {
    return invoke<InitSummary>("desktop_init_local_standard", { request });
  },
  startDesktopClient() {
    return invoke<DesktopClientSnapshot>("desktop_client_start");
  },
  shutdownDesktopClient() {
    return invoke<DesktopClientSnapshot>("desktop_client_shutdown");
  },
  addPeer(request) {
    return invoke<DesktopClientSnapshot>("desktop_peer_add", { request });
  },
  repairP2P() {
    return invoke<DesktopClientSnapshot>("desktop_p2p_repair");
  },
  fetchSessionSnapshot(sessionId, requestId) {
    return invoke<DesktopSessionSnapshot | null>("desktop_session_snapshot", {
      sessionId,
      requestId,
    });
  },
  sendChatMessage(request) {
    return invoke<ChatSendResult>("desktop_chat_send", { request });
  },
  renameConversation(request) {
    return invoke<void>("desktop_conversation_rename", { request });
  },
  saveAgentConfig(request) {
    return invoke<DesktopClientSnapshot>("desktop_agent_config_save", {
      request,
    });
  },
  saveBehaviorConfig(request) {
    return invoke<DesktopClientSnapshot>("desktop_behavior_save", { request });
  },
  saveBackendConfig(request) {
    return invoke<DesktopClientSnapshot>("desktop_backend_save", { request });
  },
  saveInferenceProfileConfig(request) {
    return invoke<DesktopClientSnapshot>("desktop_inference_profile_save", {
      request,
    });
  },
  saveToolSelectionConfig(request) {
    return invoke<DesktopClientSnapshot>("desktop_tool_selection_save", {
      request,
    });
  },
  saveToolServiceConfig(request) {
    return invoke<DesktopClientSnapshot>("desktop_tool_service_save", {
      request,
    });
  },
  testToolService(request) {
    return invoke<ToolServiceTestResult>("desktop_tool_service_test", {
      request,
    });
  },
  saveTaskConfig(request) {
    return invoke<DesktopClientSnapshot>("desktop_task_save", { request });
  },
  saveScheduleConfig(request) {
    return invoke<DesktopClientSnapshot>("desktop_schedule_save", { request });
  },
  runSchedule(request) {
    return invoke<TaskRunResult>("desktop_schedule_run", { request });
  },
  saveEventTriggerConfig(request) {
    return invoke<DesktopClientSnapshot>("desktop_event_trigger_save", {
      request,
    });
  },
  runTask(request) {
    return invoke<TaskRunResult>("desktop_task_run", { request });
  },
};

let desktopApiAdapterOverride: DesktopApiAdapter | null = null;

function desktopApiAdapter() {
  return desktopApiAdapterOverride ?? defaultDesktopApiAdapter;
}

export function setDesktopApiAdapterForTests(adapter: DesktopApiAdapter | null) {
  desktopApiAdapterOverride = adapter;
}

export async function fetchDesktopSnapshot() {
  return desktopApiAdapter().fetchDesktopSnapshot();
}

export async function initLocalStandardRuntime(request: {
  label: string;
  dangerouslyOverwrite: boolean;
  reset: boolean;
}) {
  return desktopApiAdapter().initLocalStandardRuntime(request);
}

export async function startDesktopClient() {
  return desktopApiAdapter().startDesktopClient();
}

export async function shutdownDesktopClient() {
  return desktopApiAdapter().shutdownDesktopClient();
}

export async function addPeer(request: PeerAddRequest) {
  return desktopApiAdapter().addPeer(request);
}

export async function repairP2P() {
  return desktopApiAdapter().repairP2P();
}

export async function fetchSessionSnapshot(
  sessionId: string,
  requestId?: string | null,
) {
  return desktopApiAdapter().fetchSessionSnapshot(sessionId, requestId);
}

export async function sendChatMessage(request: {
  agentDid: string;
  behaviorId?: string | null;
  sessionId?: string | null;
  content: string;
}) {
  return desktopApiAdapter().sendChatMessage(request);
}

export async function renameConversation(request: {
  sessionId: string;
  title: string;
}) {
  return desktopApiAdapter().renameConversation(request);
}

export async function saveAgentConfig(request: AgentConfigSaveRequest) {
  return desktopApiAdapter().saveAgentConfig(request);
}

export async function saveBehaviorConfig(request: BehaviorSaveRequest) {
  return desktopApiAdapter().saveBehaviorConfig(request);
}

export async function saveBackendConfig(request: BackendSaveRequest) {
  return desktopApiAdapter().saveBackendConfig(request);
}

export async function saveInferenceProfileConfig(
  request: InferenceProfileSaveRequest,
) {
  return desktopApiAdapter().saveInferenceProfileConfig(request);
}

export async function saveToolSelectionConfig(
  request: ToolSelectionSaveRequest,
) {
  return desktopApiAdapter().saveToolSelectionConfig(request);
}

export async function saveToolServiceConfig(request: ToolServiceSaveRequest) {
  return desktopApiAdapter().saveToolServiceConfig(request);
}

export async function testToolService(request: ToolServiceTestRequest) {
  return desktopApiAdapter().testToolService(request);
}

export async function saveTaskConfig(request: TaskSaveRequest) {
  return desktopApiAdapter().saveTaskConfig(request);
}

export async function saveScheduleConfig(request: ScheduleSaveRequest) {
  return desktopApiAdapter().saveScheduleConfig(request);
}

export async function runSchedule(request: ScheduleRunRequest) {
  return desktopApiAdapter().runSchedule(request);
}

export async function saveEventTriggerConfig(
  request: EventTriggerSaveRequest,
) {
  return desktopApiAdapter().saveEventTriggerConfig(request);
}

export async function runTask(request: TaskRunRequest) {
  return desktopApiAdapter().runTask(request);
}
