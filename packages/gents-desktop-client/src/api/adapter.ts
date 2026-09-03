import type { DesktopTransport } from "../transport.js";
import type { BackendHealth } from "../types/backendHealth.js";
import type {
  CascadeCancelPreview,
  ChatSendResult,
  CodexLoginResult,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  EnrollmentRequestView,
  SessionLiveDeltaView,
  InferenceProbeResult,
  InitSummary,
  InterruptRequestResult,
  MailboxItemView,
  MCPServiceHealthView,
  McpServiceProbeResult,
  NetworkStatusView,
  RequestResendResult,
  RequestTimelineView,
  SubagentTreeView,
  TaskRunResult,
  ToolServiceTestResult,
  ToolSurfaceExplanationView,
  WorkspaceListingView,
} from "../types.js";
import type {
  DesktopOperationsSnapshot,
  HeldToolCallView,
  ResolveHoldResult,
} from "../types/operations.js";
import { createDesktopInvoker } from "./invoke.js";
import type { DesktopApiAdapter, ManagedServerStatus } from "./types.js";
import type { ProviderAccountView } from "../generated/ProviderAccountView.js";

export function createDesktopApiAdapter(
  transport: DesktopTransport,
  options: { requireTauriBridge?: boolean } = {},
): DesktopApiAdapter {
  const invokeDesktop = createDesktopInvoker(
    transport,
    options.requireTauriBridge,
  );

  return {
    fetchDesktopSnapshot: () =>
      invokeDesktop<DesktopClientSnapshot>("desktop_client_snapshot"),
    initLocalStandardRuntime: (request) =>
      invokeDesktop<InitSummary>("desktop_init_local_standard", { request }),
    startDesktopClient: () =>
      invokeDesktop<DesktopClientSnapshot>("desktop_client_start"),
    shutdownDesktopClient: () =>
      invokeDesktop<DesktopClientSnapshot>("desktop_client_shutdown"),
    managedServerStatus: () =>
      invokeDesktop<ManagedServerStatus>("desktop_managed_server_status"),
    startManagedServer: (agentName) =>
      invokeDesktop<ManagedServerStatus>("desktop_managed_server_start", {
        request: { agentName },
      }),
    commitManagedServerAutoStart: (agentName) =>
      invokeDesktop<ManagedServerStatus>("desktop_managed_server_start", {
        request: { agentName },
      }),
    stopManagedServer: (disableAutoStart) =>
      invokeDesktop<ManagedServerStatus>("desktop_managed_server_stop", {
        disableAutoStart,
      }),
    setSelectedAgent: (agentDid) =>
      invokeDesktop<void>("desktop_set_selected_agent", { agentDid }),
    removePeer: (peerId) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_peer_remove", { peerId }),
    renamePeer: (peerId, label) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_peer_rename", {
        peerId,
        label,
      }),
    fetchPeerStatus: (peerId) =>
      invokeDesktop<unknown>("desktop_peer_status_fetch", {
        request: { peerId },
      }),
    requestStatusEnrollment: (serverAddress) =>
      invokeDesktop<EnrollmentRequestView>("desktop_peer_enroll_status", {
        request: { serverAddress },
      }),
    repairP2P: () => invokeDesktop<DesktopClientSnapshot>("desktop_p2p_repair"),
    listWorkspace: (subpath) =>
      invokeDesktop<WorkspaceListingView>("desktop_workspace_list", {
        subpath: subpath ?? null,
      }),
    fetchRequestTimeline: (agentDid, requestId) =>
      invokeDesktop<RequestTimelineView>("desktop_request_timeline", {
        agentDid,
        requestId,
      }),
    explainToolSurface: (agentDid, behaviorId) =>
      invokeDesktop<ToolSurfaceExplanationView>(
        "desktop_tool_surface_explain",
        { agentDid, behaviorId },
      ),
    fetchNetworkStatus: () =>
      invokeDesktop<NetworkStatusView>("desktop_network_status"),
    fetchSessionSnapshot: (sessionId, agentDid, requestId, timelinePage) =>
      invokeDesktop<DesktopSessionSnapshot | null>("desktop_session_snapshot", {
        sessionId,
        agentDid,
        requestId,
        timelineLimit: timelinePage?.limit,
        timelineBeforeItemKey: timelinePage?.beforeItemKey ?? null,
      }),
    retrySessionHydration: (sessionId, agentDid) =>
      invokeDesktop<void>("desktop_session_hydration_retry", {
        sessionId,
        agentDid: agentDid ?? null,
      }),
    fetchSessionLiveDelta: (request) =>
      invokeDesktop<SessionLiveDeltaView | null>("desktop_session_live_delta", {
        sessionId: request.sessionId,
        agentDid: request.agentDid ?? null,
        requestId: request.requestId,
        baseReconcileVersion: request.baseReconcileVersion,
        baseContentByteLen: request.baseContentByteLen,
        baseContentHash: request.baseContentHash,
        baseReasoningByteLen: request.baseReasoningByteLen,
        baseReasoningHash: request.baseReasoningHash,
      }),
    sendChatMessage: (request) =>
      invokeDesktop<ChatSendResult>("desktop_chat_send", { request }),
    listMailbox: () => invokeDesktop<MailboxItemView[]>("desktop_mailbox_list"),
    startMailboxRequest: (itemId) =>
      invokeDesktop<MailboxItemView>("desktop_mailbox_start_request", {
        request: { itemId },
      }),
    dismissMailboxItem: (itemId) =>
      invokeDesktop<void>("desktop_mailbox_dismiss", {
        request: { itemId },
      }),
    renameConversation: (request) =>
      invokeDesktop<void>("desktop_conversation_rename", { request }),
    resendRequest: (requestId) =>
      invokeDesktop<RequestResendResult>("desktop_request_resend", {
        requestId,
      }),
    retryRequest: (requestId) =>
      invokeDesktop<ChatSendResult>("desktop_request_retry", {
        requestId,
      }),
    saveAgentConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_agent_config_save", {
        request,
      }),
    saveBehaviorConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_behavior_save", {
        request,
      }),
    saveSkillConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_skill_save", { request }),
    deleteSkillConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_skill_delete", { request }),
    deleteTaskConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_task_delete", { request }),
    deleteScheduleConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_schedule_delete", {
        request,
      }),
    deleteEventTriggerConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_event_trigger_delete", {
        request,
      }),
    deleteBackendConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_backend_delete", {
        request,
      }),
    deleteInferenceProfileConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_inference_profile_delete", {
        request,
      }),
    deleteToolSelectionConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_tool_selection_delete", {
        request,
      }),
    deleteToolServiceConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_tool_service_delete", {
        request,
      }),
    deleteBehaviorConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_behavior_delete", {
        request,
      }),
    saveBackendConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_backend_save", { request }),
    probeInferenceEndpoint: (endpoint) =>
      invokeDesktop<InferenceProbeResult>("desktop_probe_inference_endpoint", {
        request: { endpoint },
      }),
    codexLogin: (agentDid, provider) =>
      invokeDesktop<CodexLoginResult>("desktop_codex_login", {
        request: { agentDid, provider: provider ?? null },
      }),
    cancelCodexLogin: () => invokeDesktop<void>("desktop_codex_login_cancel"),
    grokLogin: (agentDid, provider) =>
      invokeDesktop<import("../generated/GrokLoginResult.js").GrokLoginResult>(
        "desktop_grok_login",
        {
          request: { agentDid, provider: provider ?? null },
        },
      ),
    cancelGrokLogin: () => invokeDesktop<void>("desktop_grok_login_cancel"),
    listProviderAccounts: (agentDid) =>
      invokeDesktop<ProviderAccountView[]>("desktop_provider_accounts_list", {
        request: { agentDid },
      }),
    disconnectProviderAccount: (agentDid, credentialId) =>
      invokeDesktop<void>("desktop_provider_account_disconnect", {
        request: { agentDid, credentialId },
      }),
    saveInferenceProfileConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_inference_profile_save", {
        request,
      }),
    saveToolSelectionConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_tool_selection_save", {
        request,
      }),
    saveToolServiceConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_tool_service_save", {
        request,
      }),
    testToolService: (request) =>
      invokeDesktop<ToolServiceTestResult>("desktop_tool_service_test", {
        request,
      }),
    saveTaskConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_task_save", { request }),
    saveScheduleConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_schedule_save", {
        request,
      }),
    runSchedule: (request) =>
      invokeDesktop<TaskRunResult>("desktop_schedule_run", { request }),
    saveEventTriggerConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_event_trigger_save", {
        request,
      }),
    runTask: (request) =>
      invokeDesktop<TaskRunResult>("desktop_task_run", { request }),
    listSubagentTree: (request) =>
      invokeDesktop<SubagentTreeView>("desktop_list_subagent_tree", {
        request,
      }),
    listBackendsWithHealth: () =>
      invokeDesktop<BackendHealth[]>("desktop_list_backends_with_health"),
    listMcpServicesWithHealth: () =>
      invokeDesktop<MCPServiceHealthView[]>(
        "desktop_list_mcp_services_with_health",
      ),
    probeMcpService: (serviceId) =>
      invokeDesktop<McpServiceProbeResult>("desktop_probe_mcp_service", {
        request: { serviceId },
      }),
    fetchOperationsSnapshot: (request) =>
      invokeDesktop<DesktopOperationsSnapshot>("desktop_operations_snapshot", {
        request,
      }),
    previewInterruptCascade: (request) =>
      invokeDesktop<CascadeCancelPreview>("desktop_preview_interrupt_cascade", {
        request,
      }),
    interruptRequest: (request) =>
      invokeDesktop<InterruptRequestResult>("desktop_interrupt_request", {
        request,
      }),
    listToolCallHolds: (agentDid) =>
      invokeDesktop<HeldToolCallView[]>("desktop_list_tool_call_holds", {
        request: { agentDid },
      }),
    resolveToolCallHold: (request) =>
      invokeDesktop<ResolveHoldResult>("desktop_resolve_tool_call_hold", {
        request,
      }),
  };
}
