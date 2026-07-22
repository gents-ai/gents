import { invoke } from "@tauri-apps/api/core";

import type { BackendHealth } from "../components/backendHealth/types";
import type {
  AgentConfigSaveRequest,
  BackendSaveRequest,
  BehaviorSaveRequest,
  CascadeCancelPreview,
  ChatSendResult,
  DesktopClientSnapshot,
  DesktopInterruptRequestRequest,
  DesktopListSubagentTreeRequest,
  DesktopPreviewInterruptCascadeRequest,
  DesktopSessionSnapshot,
  EventTriggerSaveRequest,
  InferenceProfileSaveRequest,
  InitSummary,
  InterruptRequestResult,
  MCPServiceHealthView,
  McpServiceProbeResult,
  PeerAddRequest,
  ScheduleRunRequest,
  ScheduleSaveRequest,
  SkillDeleteRequest,
  TaskDeleteRequest,
  ScheduleDeleteRequest,
  EventTriggerDeleteRequest,
  BackendDeleteRequest,
  InferenceProfileDeleteRequest,
  ToolSelectionDeleteRequest,
  ToolServiceDeleteRequest,
  BehaviorDeleteRequest,
  SkillSaveRequest,
  SubagentTreeView,
  TaskRunRequest,
  TaskRunResult,
  TaskSaveRequest,
  ToolSelectionSaveRequest,
  ToolServiceSaveRequest,
  ToolServiceTestRequest,
  ToolServiceTestResult,
  WorkspaceListingView,
  SessionForkResult,
  RequestResendResult,
  RequestTimelineView,
  ToolSurfaceExplanationView,
  NetworkStatusView,
} from "./types";
import type {
  DesktopOperationsSnapshot,
  DesktopOperationsSnapshotRequest,
  DesktopResolveHoldRequest,
  HeldToolCallView,
  ResolveHoldResult,
} from "./types/operations";

type TauriInternalsWindow = Window & {
  __TAURI_INTERNALS__?: {
    invoke?: unknown;
  };
};

function hasTauriInvokeBridge() {
  return (
    typeof window !== "undefined" &&
    typeof (window as TauriInternalsWindow).__TAURI_INTERNALS__?.invoke === "function"
  );
}

function invokeDesktop<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (!hasTauriInvokeBridge()) {
    return Promise.reject(
      new Error(
        "Desktop native bridge is unavailable. Open this screen in the Tauri desktop app to save agent connections.",
      ),
    );
  }

  return invoke<T>(command, args);
}

type InitSummaryWire = InitSummary & {
  status_endpoint?: string | null;
  agent_home?: string;
  desktop_home?: string;
  peer_directory?: string;
  agent_name?: string;
  agent_did?: string;
  p2p_transport?: string;
  p2p_peer_id?: string;
  p2p_listen_address?: string;
  peer_record_id?: string;
  next_steps?: string[];
};

function normalizeInitSummary(summary: InitSummaryWire): InitSummary {
  return {
    status: summary.status,
    source: summary.source,
    statusEndpoint: summary.statusEndpoint ?? summary.status_endpoint ?? null,
    agentHome: summary.agentHome ?? summary.agent_home ?? "",
    desktopHome: summary.desktopHome ?? summary.desktop_home ?? "",
    peerDirectory: summary.peerDirectory ?? summary.peer_directory ?? "",
    label: summary.label,
    agentName: summary.agentName ?? summary.agent_name ?? "",
    agentDid: summary.agentDid ?? summary.agent_did ?? "",
    graphql: summary.graphql,
    p2pTransport: summary.p2pTransport ?? summary.p2p_transport ?? "",
    p2pPeerId: summary.p2pPeerId ?? summary.p2p_peer_id ?? "",
    p2pListenAddress: summary.p2pListenAddress ?? summary.p2p_listen_address ?? "",
    peerRecordId: summary.peerRecordId ?? summary.peer_record_id ?? "",
    nextSteps: summary.nextSteps ?? summary.next_steps ?? [],
  };
}

export type DesktopApiAdapter = {
  fetchDesktopSnapshot: () => Promise<DesktopClientSnapshot>;
  initLocalStandardRuntime: (request: {
    label: string;
    dangerouslyOverwrite: boolean;
    reset: boolean;
  }) => Promise<InitSummary>;
  startDesktopClient: () => Promise<DesktopClientSnapshot>;
  shutdownDesktopClient: () => Promise<DesktopClientSnapshot>;
  setSelectedAgent: (agentDid: string | null) => Promise<void>;
  addPeer: (request: PeerAddRequest) => Promise<DesktopClientSnapshot>;
  removePeer: (peerId: string) => Promise<DesktopClientSnapshot>;
  renamePeer: (peerId: string, label: string) => Promise<DesktopClientSnapshot>;
  fetchPeerStatus: (serverAddress: string) => Promise<unknown>;
  repairP2P: () => Promise<DesktopClientSnapshot>;
  listWorkspace: (subpath?: string | null) => Promise<WorkspaceListingView>;
  fetchRequestTimeline: (
    agentDid: string,
    requestId: string,
  ) => Promise<RequestTimelineView>;
  explainToolSurface: (
    agentDid: string,
    behaviorId: string,
  ) => Promise<ToolSurfaceExplanationView>;
  fetchNetworkStatus: () => Promise<NetworkStatusView>;
  fetchSessionSnapshot: (
    sessionId: string,
    agentDid?: string | null,
    requestId?: string | null,
  ) => Promise<DesktopSessionSnapshot | null>;
  sendChatMessage: (request: {
    agentDid: string;
    behaviorId?: string | null;
    sessionId?: string | null;
    content: string;
  }) => Promise<ChatSendResult>;
  renameConversation: (request: { sessionId: string; title: string }) => Promise<void>;
  forkSession: (request: {
    agentDid: string;
    sessionId: string;
    atUserTurn: number;
    behaviorId?: string | null;
  }) => Promise<SessionForkResult>;
  resendRequest: (requestId: string) => Promise<RequestResendResult>;
  saveAgentConfig: (request: AgentConfigSaveRequest) => Promise<DesktopClientSnapshot>;
  saveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<DesktopClientSnapshot>;
  saveSkillConfig: (request: SkillSaveRequest) => Promise<DesktopClientSnapshot>;
  deleteSkillConfig: (request: SkillDeleteRequest) => Promise<DesktopClientSnapshot>;
  deleteTaskConfig: (request: TaskDeleteRequest) => Promise<DesktopClientSnapshot>;
  deleteScheduleConfig: (
    request: ScheduleDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteEventTriggerConfig: (
    request: EventTriggerDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteBackendConfig: (
    request: BackendDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteInferenceProfileConfig: (
    request: InferenceProfileDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteToolSelectionConfig: (
    request: ToolSelectionDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteToolServiceConfig: (
    request: ToolServiceDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  deleteBehaviorConfig: (
    request: BehaviorDeleteRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveBackendConfig: (request: BackendSaveRequest) => Promise<DesktopClientSnapshot>;
  saveInferenceProfileConfig: (
    request: InferenceProfileSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveToolSelectionConfig: (
    request: ToolSelectionSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  saveToolServiceConfig: (
    request: ToolServiceSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  testToolService: (request: ToolServiceTestRequest) => Promise<ToolServiceTestResult>;
  saveTaskConfig: (request: TaskSaveRequest) => Promise<DesktopClientSnapshot>;
  saveScheduleConfig: (request: ScheduleSaveRequest) => Promise<DesktopClientSnapshot>;
  runSchedule: (request: ScheduleRunRequest) => Promise<TaskRunResult>;
  saveEventTriggerConfig: (
    request: EventTriggerSaveRequest,
  ) => Promise<DesktopClientSnapshot>;
  runTask: (request: TaskRunRequest) => Promise<TaskRunResult>;
  listSubagentTree: (
    request: DesktopListSubagentTreeRequest,
  ) => Promise<SubagentTreeView>;
  listBackendsWithHealth: () => Promise<BackendHealth[]>;
  listMcpServicesWithHealth: () => Promise<MCPServiceHealthView[]>;
  probeMcpService: (serviceId: string) => Promise<McpServiceProbeResult>;
  fetchOperationsSnapshot: (
    request: DesktopOperationsSnapshotRequest,
  ) => Promise<DesktopOperationsSnapshot>;
  previewInterruptCascade: (
    request: DesktopPreviewInterruptCascadeRequest,
  ) => Promise<CascadeCancelPreview>;
  interruptRequest: (
    request: DesktopInterruptRequestRequest,
  ) => Promise<InterruptRequestResult>;
  listToolCallHolds: (agentDid: string) => Promise<HeldToolCallView[]>;
  resolveToolCallHold: (
    request: DesktopResolveHoldRequest,
  ) => Promise<ResolveHoldResult>;
};

const defaultDesktopApiAdapter: DesktopApiAdapter = {
  fetchDesktopSnapshot() {
    return invokeDesktop<DesktopClientSnapshot>("desktop_client_snapshot");
  },
  initLocalStandardRuntime(request) {
    return invokeDesktop<InitSummary>("desktop_init_local_standard", { request });
  },
  startDesktopClient() {
    return invokeDesktop<DesktopClientSnapshot>("desktop_client_start");
  },
  shutdownDesktopClient() {
    return invokeDesktop<DesktopClientSnapshot>("desktop_client_shutdown");
  },
  setSelectedAgent(agentDid) {
    return invokeDesktop<void>("desktop_set_selected_agent", { agentDid });
  },
  addPeer(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_peer_add", { request });
  },
  removePeer(peerId) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_peer_remove", { peerId });
  },
  renamePeer(peerId, label) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_peer_rename", {
      peerId,
      label,
    });
  },
  fetchPeerStatus(serverAddress) {
    return invokeDesktop<unknown>("desktop_peer_status_fetch", {
      request: { serverAddress },
    });
  },
  repairP2P() {
    return invokeDesktop<DesktopClientSnapshot>("desktop_p2p_repair");
  },
  listWorkspace(subpath) {
    return invokeDesktop<WorkspaceListingView>("desktop_workspace_list", {
      subpath: subpath ?? null,
    });
  },
  fetchRequestTimeline(agentDid, requestId) {
    return invokeDesktop<RequestTimelineView>("desktop_request_timeline", {
      agentDid,
      requestId,
    });
  },
  explainToolSurface(agentDid, behaviorId) {
    return invokeDesktop<ToolSurfaceExplanationView>("desktop_tool_surface_explain", {
      agentDid,
      behaviorId,
    });
  },
  fetchNetworkStatus() {
    return invokeDesktop<NetworkStatusView>("desktop_network_status");
  },
  fetchSessionSnapshot(sessionId, agentDid, requestId) {
    return invokeDesktop<DesktopSessionSnapshot | null>("desktop_session_snapshot", {
      sessionId,
      agentDid,
      requestId,
    });
  },
  sendChatMessage(request) {
    return invokeDesktop<ChatSendResult>("desktop_chat_send", { request });
  },
  renameConversation(request) {
    return invokeDesktop<void>("desktop_conversation_rename", { request });
  },
  forkSession(request) {
    return invokeDesktop<SessionForkResult>("desktop_session_fork", {
      agentDid: request.agentDid,
      sessionId: request.sessionId,
      atUserTurn: request.atUserTurn,
      behaviorId: request.behaviorId ?? null,
    });
  },
  resendRequest(requestId) {
    return invokeDesktop<RequestResendResult>("desktop_request_resend", { requestId });
  },
  saveAgentConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_agent_config_save", {
      request,
    });
  },
  saveBehaviorConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_behavior_save", { request });
  },
  saveSkillConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_skill_save", { request });
  },
  deleteSkillConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_skill_delete", { request });
  },
  deleteTaskConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_task_delete", { request });
  },
  deleteScheduleConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_schedule_delete", { request });
  },
  deleteEventTriggerConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_event_trigger_delete", {
      request,
    });
  },
  deleteBackendConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_backend_delete", { request });
  },
  deleteInferenceProfileConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_inference_profile_delete", {
      request,
    });
  },
  deleteToolSelectionConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_tool_selection_delete", {
      request,
    });
  },
  deleteToolServiceConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_tool_service_delete", {
      request,
    });
  },
  deleteBehaviorConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_behavior_delete", { request });
  },
  saveBackendConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_backend_save", { request });
  },
  saveInferenceProfileConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_inference_profile_save", {
      request,
    });
  },
  saveToolSelectionConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_tool_selection_save", {
      request,
    });
  },
  saveToolServiceConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_tool_service_save", {
      request,
    });
  },
  testToolService(request) {
    return invokeDesktop<ToolServiceTestResult>("desktop_tool_service_test", {
      request,
    });
  },
  saveTaskConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_task_save", { request });
  },
  saveScheduleConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_schedule_save", {
      request,
    });
  },
  runSchedule(request) {
    return invokeDesktop<TaskRunResult>("desktop_schedule_run", { request });
  },
  saveEventTriggerConfig(request) {
    return invokeDesktop<DesktopClientSnapshot>("desktop_event_trigger_save", {
      request,
    });
  },
  runTask(request) {
    return invokeDesktop<TaskRunResult>("desktop_task_run", { request });
  },
  listSubagentTree(request) {
    return invokeDesktop<SubagentTreeView>("desktop_list_subagent_tree", {
      request,
    });
  },
  listBackendsWithHealth() {
    return invokeDesktop<BackendHealth[]>("desktop_list_backends_with_health");
  },
  listMcpServicesWithHealth() {
    return invokeDesktop<MCPServiceHealthView[]>(
      "desktop_list_mcp_services_with_health",
    );
  },
  probeMcpService(serviceId) {
    return invokeDesktop<McpServiceProbeResult>("desktop_probe_mcp_service", {
      request: { serviceId },
    });
  },
  fetchOperationsSnapshot(request) {
    return invokeDesktop<DesktopOperationsSnapshot>("desktop_operations_snapshot", {
      request,
    });
  },
  previewInterruptCascade(request) {
    return invokeDesktop<CascadeCancelPreview>("desktop_preview_interrupt_cascade", {
      request,
    });
  },
  interruptRequest(request) {
    return invokeDesktop<InterruptRequestResult>("desktop_interrupt_request", {
      request,
    });
  },
  listToolCallHolds(agentDid) {
    return invokeDesktop<HeldToolCallView[]>("desktop_list_tool_call_holds", {
      request: { agentDid },
    });
  },
  resolveToolCallHold(request) {
    return invokeDesktop<ResolveHoldResult>("desktop_resolve_tool_call_hold", {
      request,
    });
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
  return normalizeInitSummary(
    await desktopApiAdapter().initLocalStandardRuntime(request),
  );
}

export async function startDesktopClient() {
  return desktopApiAdapter().startDesktopClient();
}

export async function shutdownDesktopClient() {
  return desktopApiAdapter().shutdownDesktopClient();
}

export async function setSelectedAgent(agentDid: string | null) {
  return desktopApiAdapter().setSelectedAgent(agentDid);
}

export async function addPeer(request: PeerAddRequest) {
  return desktopApiAdapter().addPeer(request);
}

export async function removePeer(peerId: string) {
  return desktopApiAdapter().removePeer(peerId);
}

export async function renamePeer(peerId: string, label: string) {
  return desktopApiAdapter().renamePeer(peerId, label);
}

export async function fetchPeerStatus(serverAddress: string) {
  return desktopApiAdapter().fetchPeerStatus(serverAddress);
}

export async function repairP2P() {
  return desktopApiAdapter().repairP2P();
}

export async function listWorkspace(subpath?: string | null) {
  return desktopApiAdapter().listWorkspace(subpath);
}

export async function fetchRequestTimeline(agentDid: string, requestId: string) {
  return desktopApiAdapter().fetchRequestTimeline(agentDid, requestId);
}

export async function explainToolSurface(agentDid: string, behaviorId: string) {
  return desktopApiAdapter().explainToolSurface(agentDid, behaviorId);
}

export async function fetchNetworkStatus() {
  return desktopApiAdapter().fetchNetworkStatus();
}

export async function fetchSessionSnapshot(
  sessionId: string,
  agentDid?: string | null,
  requestId?: string | null,
) {
  return desktopApiAdapter().fetchSessionSnapshot(sessionId, agentDid, requestId);
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

export async function forkSession(request: {
  agentDid: string;
  sessionId: string;
  atUserTurn: number;
  behaviorId?: string | null;
}) {
  return desktopApiAdapter().forkSession(request);
}

export async function resendRequest(requestId: string) {
  return desktopApiAdapter().resendRequest(requestId);
}

export async function saveAgentConfig(request: AgentConfigSaveRequest) {
  return desktopApiAdapter().saveAgentConfig(request);
}

export async function saveBehaviorConfig(request: BehaviorSaveRequest) {
  return desktopApiAdapter().saveBehaviorConfig(request);
}

export async function saveSkillConfig(request: SkillSaveRequest) {
  return desktopApiAdapter().saveSkillConfig(request);
}

export async function deleteSkillConfig(request: SkillDeleteRequest) {
  return desktopApiAdapter().deleteSkillConfig(request);
}

export async function deleteTaskConfig(request: TaskDeleteRequest) {
  return desktopApiAdapter().deleteTaskConfig(request);
}

export async function deleteScheduleConfig(request: ScheduleDeleteRequest) {
  return desktopApiAdapter().deleteScheduleConfig(request);
}

export async function deleteEventTriggerConfig(request: EventTriggerDeleteRequest) {
  return desktopApiAdapter().deleteEventTriggerConfig(request);
}

export async function deleteBackendConfig(request: BackendDeleteRequest) {
  return desktopApiAdapter().deleteBackendConfig(request);
}

export async function deleteInferenceProfileConfig(
  request: InferenceProfileDeleteRequest,
) {
  return desktopApiAdapter().deleteInferenceProfileConfig(request);
}

export async function deleteToolSelectionConfig(request: ToolSelectionDeleteRequest) {
  return desktopApiAdapter().deleteToolSelectionConfig(request);
}

export async function deleteToolServiceConfig(request: ToolServiceDeleteRequest) {
  return desktopApiAdapter().deleteToolServiceConfig(request);
}

export async function deleteBehaviorConfig(request: BehaviorDeleteRequest) {
  return desktopApiAdapter().deleteBehaviorConfig(request);
}

export async function saveBackendConfig(request: BackendSaveRequest) {
  return desktopApiAdapter().saveBackendConfig(request);
}

export async function saveInferenceProfileConfig(request: InferenceProfileSaveRequest) {
  return desktopApiAdapter().saveInferenceProfileConfig(request);
}

export async function saveToolSelectionConfig(request: ToolSelectionSaveRequest) {
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

export async function saveEventTriggerConfig(request: EventTriggerSaveRequest) {
  return desktopApiAdapter().saveEventTriggerConfig(request);
}

export async function runTask(request: TaskRunRequest) {
  return desktopApiAdapter().runTask(request);
}

export async function listSubagentTree(request: DesktopListSubagentTreeRequest) {
  return desktopApiAdapter().listSubagentTree(request);
}

export async function listBackendsWithHealth() {
  return desktopApiAdapter().listBackendsWithHealth();
}

export async function listMcpServicesWithHealth() {
  return desktopApiAdapter().listMcpServicesWithHealth();
}

export async function probeMcpService(serviceId: string) {
  return desktopApiAdapter().probeMcpService(serviceId);
}

export async function fetchOperationsSnapshot(
  request: DesktopOperationsSnapshotRequest,
) {
  return desktopApiAdapter().fetchOperationsSnapshot(request);
}

export async function previewInterruptCascade(
  request: DesktopPreviewInterruptCascadeRequest,
) {
  return desktopApiAdapter().previewInterruptCascade(request);
}

export async function interruptRequest(request: DesktopInterruptRequestRequest) {
  return desktopApiAdapter().interruptRequest(request);
}

export async function listToolCallHolds(agentDid: string) {
  return desktopApiAdapter().listToolCallHolds(agentDid);
}

export async function resolveToolCallHold(request: DesktopResolveHoldRequest) {
  return desktopApiAdapter().resolveToolCallHold(request);
}
