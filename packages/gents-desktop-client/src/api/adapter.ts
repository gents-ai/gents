import type { DesktopTransport } from "../transport.js";
import type { BackendHealth } from "../types/backendHealth.js";
import type {
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
  SessionProvenanceView,
  TaskRunResult,
  ToolServiceTestResult,
  ToolSurfaceExplanationView,
  WorkspaceListingView,
} from "../types.js";
import type { DesktopOperationsSnapshot } from "../types/operations.js";
import { createDesktopInvoker } from "./invoke.js";
import { createHostAccessCommands } from "./hostAccess.js";
import { createManagedServerCommands } from "./managedServer.js";
import { createPackCommands } from "./packs.js";
import { createProviderAccountCommands } from "./providerAccounts.js";
import type { DesktopApiAdapter } from "./types.js";
import type { InferenceSetupCatalog } from "../generated/InferenceSetupCatalog.js";
import type { InferenceDiscoveryResult } from "../generated/InferenceDiscoveryResult.js";
import type { InferenceModelRecommendation } from "../generated/InferenceModelRecommendation.js";
import type { GrokLoginResult } from "../generated/GrokLoginResult.js";
import type { ClaudeLoginResult } from "../generated/ClaudeLoginResult.js";

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
    ...createManagedServerCommands(invokeDesktop),
    quitDesktop: () => invokeDesktop<void>("desktop_app_quit"),
    openDbExplorer: () => invokeDesktop<string>("desktop_open_db_explorer"),
    openExternalUrl: (url) =>
      invokeDesktop<void>("desktop_open_external_url", { url }),
    setSelectedNode: (nodeDid) =>
      invokeDesktop<void>("desktop_set_selected_node", { nodeDid }),
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
    listWorkspace: (subpath) =>
      invokeDesktop<WorkspaceListingView>("desktop_workspace_list", {
        subpath: subpath ?? null,
      }),
    fetchRequestTimeline: (nodeDid, requestId) =>
      invokeDesktop<RequestTimelineView>("desktop_request_timeline", {
        nodeDid,
        requestId,
      }),
    explainToolSurface: (nodeDid, agentId) =>
      invokeDesktop<ToolSurfaceExplanationView>(
        "desktop_tool_surface_explain",
        { nodeDid, agentId },
      ),
    fetchNetworkStatus: () =>
      invokeDesktop<NetworkStatusView>("desktop_network_status"),
    fetchSessionSnapshot: (sessionId, nodeDid, requestId, timelinePage) =>
      invokeDesktop<DesktopSessionSnapshot | null>("desktop_session_snapshot", {
        sessionId,
        nodeDid,
        requestId,
        timelineLimit: timelinePage?.limit,
        timelineBeforeItemKey: timelinePage?.beforeItemKey ?? null,
      }),
    retrySessionHydration: (sessionId, nodeDid) =>
      invokeDesktop<void>("desktop_session_hydration_retry", {
        sessionId,
        nodeDid: nodeDid ?? null,
      }),
    fetchSessionLiveDelta: (request) =>
      invokeDesktop<SessionLiveDeltaView | null>("desktop_session_live_delta", {
        sessionId: request.sessionId,
        nodeDid: request.nodeDid ?? null,
        requestId: request.requestId,
        baseLiveCursor: request.baseLiveCursor,
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
    renameSession: (request) =>
      invokeDesktop<void>("desktop_session_rename", { request }),
    resendRequest: (requestId, nodeDid) =>
      invokeDesktop<RequestResendResult>("desktop_request_resend", {
        requestId,
        nodeDid,
      }),
    retryRequest: (requestId, nodeDid) =>
      invokeDesktop<ChatSendResult>("desktop_request_retry", {
        requestId,
        nodeDid,
      }),
    applyConfigComponents: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_config_components_apply", {
        request,
      }),
    patchConfigComponents: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_config_components_patch", {
        request,
      }),
    saveNodeConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_node_config_save", {
        request,
      }),
    setDefaultAgent: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_default_agent_set", {
        request,
      }),
    saveAgentConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_agent_save", {
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
    saveEventSourceConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_event_source_save", {
        request,
      }),
    deleteEventSourceConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_event_source_delete", {
        request,
      }),
    deleteTriggerConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_trigger_delete", {
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
    deleteToolsConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_tools_delete", {
        request,
      }),
    deleteToolServiceConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_tool_service_delete", {
        request,
      }),
    deleteAgentConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_agent_delete", {
        request,
      }),
    deleteContextConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_context_delete", {
        request,
      }),
    saveBackendConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_backend_save", { request }),
    probeInferenceEndpoint: (endpoint) =>
      invokeDesktop<InferenceProbeResult>("desktop_probe_inference_endpoint", {
        request: { endpoint },
      }),
    getInferenceSetupCatalog: () =>
      invokeDesktop<InferenceSetupCatalog>("desktop_inference_setup_catalog"),
    discoverInferenceModels: (request) =>
      invokeDesktop<InferenceDiscoveryResult>(
        "desktop_inference_models_discover",
        {
          request,
        },
      ),
    getInferenceModelRecommendation: (request) =>
      invokeDesktop<InferenceModelRecommendation>(
        "desktop_inference_model_recommendation",
        { request },
      ),
    getInferenceBackendRecommendation: (request) =>
      invokeDesktop<InferenceModelRecommendation>(
        "desktop_inference_backend_recommendation",
        { request },
      ),
    codexLogin: (nodeDid, provider, label) =>
      invokeDesktop<CodexLoginResult>("desktop_codex_login", {
        request: { nodeDid, provider: provider ?? null, label: label ?? null },
      }),
    cancelCodexLogin: () => invokeDesktop<void>("desktop_codex_login_cancel"),
    grokLogin: (nodeDid, provider, label) =>
      invokeDesktop<GrokLoginResult>("desktop_grok_login", {
        request: { nodeDid, provider: provider ?? null, label: label ?? null },
      }),
    cancelGrokLogin: () => invokeDesktop<void>("desktop_grok_login_cancel"),
    claudeLogin: (nodeDid, provider, label) =>
      invokeDesktop<ClaudeLoginResult>("desktop_claude_login", {
        request: { nodeDid, provider: provider ?? null, label: label ?? null },
      }),
    cancelClaudeLogin: () => invokeDesktop<void>("desktop_claude_login_cancel"),
    watchProviderLoginUrl: (provider, onUrl) =>
      transport.listenProviderLoginUrl?.(provider, onUrl) ??
      Promise.resolve(() => {}),
    ...createProviderAccountCommands(invokeDesktop),
    ...createPackCommands(invokeDesktop),
    ...createHostAccessCommands(invokeDesktop),
    saveInferenceProfileConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_inference_profile_save", {
        request,
      }),
    saveToolsConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_tools_save", {
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
    saveTriggerConfig: (request) =>
      invokeDesktop<DesktopClientSnapshot>("desktop_trigger_save", {
        request,
      }),
    runTask: (request) =>
      invokeDesktop<TaskRunResult>("desktop_task_run", { request }),
    sessionProvenance: (request) =>
      invokeDesktop<SessionProvenanceView>("desktop_session_provenance", {
        request,
      }),
    listBackendsWithHealth: () =>
      invokeDesktop<BackendHealth[]>("desktop_list_backends_with_health"),
    listMcpServicesWithHealth: () =>
      invokeDesktop<MCPServiceHealthView[]>(
        "desktop_list_mcp_services_with_health",
      ),
    probeMcpService: (nodeDid, serviceId) =>
      invokeDesktop<McpServiceProbeResult>("desktop_probe_mcp_service", {
        request: { nodeDid, serviceId },
      }),
    fetchOperationsSnapshot: (request) =>
      invokeDesktop<DesktopOperationsSnapshot>("desktop_operations_snapshot", {
        request,
      }),
    interruptRequest: (request) =>
      invokeDesktop<InterruptRequestResult>("desktop_interrupt_request", {
        request,
      }),
  };
}
