import type { DesktopTransport } from "../transport.js";
import { createHostAccessCommands } from "./hostAccess.js";
import { createDesktopInvoker } from "./invoke.js";
import { createManagedServerCommands } from "./managedServer.js";
import { createPackCommands } from "./packs.js";
import { createUxPluginCommands } from "./uxPlugins.js";
import { createProviderAccountCommands } from "./providerAccounts.js";
import type { DesktopApiAdapter } from "./types.js";

export function createDesktopApiAdapter(
  transport: DesktopTransport,
  options: { requireTauriBridge?: boolean } = {},
): DesktopApiAdapter {
  const invokeDesktop = createDesktopInvoker(
    transport,
    options.requireTauriBridge,
  );

  return {
    readAttachment: (path) =>
      invokeDesktop("desktop_attachment_read", { path }),
    requestAttachments: (nodeDid, sessionId, requestId, messageKey) =>
      invokeDesktop("desktop_request_attachments", {
        nodeDid,
        sessionId,
        requestId,
        messageKey,
      }),
    fetchSessionGoal: (nodeDid, sessionId) =>
      invokeDesktop("desktop_session_goal", { nodeDid, sessionId }),
    controlGoal: (request) =>
      invokeDesktop("desktop_goal_control", { request }),
    fetchDesktopSnapshot: () => invokeDesktop("desktop_client_snapshot"),
    initLocalStandardRuntime: (request) =>
      invokeDesktop("desktop_init_local_standard", { request }),
    startDesktopClient: () => invokeDesktop("desktop_client_start"),
    shutdownDesktopClient: () => invokeDesktop("desktop_client_shutdown"),
    ...createManagedServerCommands(invokeDesktop),
    quitDesktop: () => invokeDesktop("desktop_app_quit"),
    openDbExplorer: () => invokeDesktop("desktop_open_db_explorer"),
    openExternalUrl: (url) =>
      invokeDesktop("desktop_open_external_url", { url }),
    setSelectedNode: (nodeDid) =>
      invokeDesktop("desktop_set_selected_node", { nodeDid }),
    removePeer: (peerId) => invokeDesktop("desktop_peer_remove", { peerId }),
    renamePeer: (peerId, label) =>
      invokeDesktop("desktop_peer_rename", {
        peerId,
        label,
      }),
    fetchPeerStatus: (peerId) =>
      invokeDesktop("desktop_peer_status_fetch", {
        request: { peerId },
      }),
    requestStatusEnrollment: (serverAddress) =>
      invokeDesktop("desktop_peer_enroll_status", {
        request: { serverAddress },
      }),
    listWorkspace: (subpath) =>
      invokeDesktop("desktop_workspace_list", {
        subpath: subpath ?? null,
      }),
    fetchRequestTimeline: (nodeDid, requestId) =>
      invokeDesktop("desktop_request_timeline", {
        nodeDid,
        requestId,
      }),
    explainToolSurface: (nodeDid, agentId) =>
      invokeDesktop("desktop_tool_surface_explain", { nodeDid, agentId }),
    fetchNetworkStatus: () => invokeDesktop("desktop_network_status"),
    fetchSessionSnapshot: (sessionId, nodeDid, requestId, timelinePage) =>
      invokeDesktop("desktop_session_snapshot", {
        sessionId,
        nodeDid,
        requestId,
        timelineLimit: timelinePage?.limit,
        timelineBeforeItemKey: timelinePage?.beforeItemKey ?? null,
      }),
    retrySessionHydration: (sessionId, nodeDid) =>
      invokeDesktop("desktop_session_hydration_retry", {
        sessionId,
        nodeDid: nodeDid ?? null,
      }),
    fetchSessionRows: (request) =>
      invokeDesktop("desktop_session_rows", {
        sessionId: request.sessionId,
        nodeDid: request.nodeDid ?? null,
        window: request.window,
        limit: request.limit ?? null,
      }),
    fetchSessionLiveDelta: (request) =>
      invokeDesktop("desktop_session_live_delta", {
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
      invokeDesktop("desktop_chat_send", { request }),
    recoverChatSubmission: (query) =>
      invokeDesktop("desktop_chat_submission_recover", { query }),
    acknowledgeChatSubmission: (receipt) =>
      invokeDesktop("desktop_chat_submission_acknowledge", { receipt }),
    steerPendingMessage: (request) =>
      invokeDesktop("desktop_pending_message_steer", { request }),
    editPendingQueue: (request) =>
      invokeDesktop("desktop_pending_queue_edit", {
        request,
      }),
    listMailbox: () => invokeDesktop("desktop_mailbox_list"),
    startMailboxRequest: (itemId) =>
      invokeDesktop("desktop_mailbox_start_request", {
        request: { itemId },
      }),
    dismissMailboxItem: (itemId) =>
      invokeDesktop("desktop_mailbox_dismiss", {
        request: { itemId },
      }),
    forkSessionBeforeMessage: (request) =>
      invokeDesktop("desktop_session_fork_before_message", { request }),
    renameSession: (request) =>
      invokeDesktop("desktop_session_rename", { request }),
    resendRequest: (requestId, nodeDid) =>
      invokeDesktop("desktop_request_resend", {
        requestId,
        nodeDid,
      }),
    retryRequest: (requestId, nodeDid) =>
      invokeDesktop("desktop_request_retry", {
        requestId,
        nodeDid,
      }),
    applyConfigComponents: (request) =>
      invokeDesktop("desktop_config_components_apply", {
        request,
      }),
    patchConfigComponents: (request) =>
      invokeDesktop("desktop_config_components_patch", {
        request,
      }),
    saveNodeConfig: (request) =>
      invokeDesktop("desktop_node_config_save", {
        request,
      }),
    setDefaultAgent: (request) =>
      invokeDesktop("desktop_default_agent_set", {
        request,
      }),
    saveAgentConfig: (request) =>
      invokeDesktop("desktop_agent_save", {
        request,
      }),
    saveSkillConfig: (request) =>
      invokeDesktop("desktop_skill_save", { request }),
    deleteSkillConfig: (request) =>
      invokeDesktop("desktop_skill_delete", { request }),
    deleteTaskConfig: (request) =>
      invokeDesktop("desktop_task_delete", { request }),
    deleteScheduleConfig: (request) =>
      invokeDesktop("desktop_schedule_delete", {
        request,
      }),
    saveEventSourceConfig: (request) =>
      invokeDesktop("desktop_event_source_save", {
        request,
      }),
    deleteEventSourceConfig: (request) =>
      invokeDesktop("desktop_event_source_delete", {
        request,
      }),
    deleteTriggerConfig: (request) =>
      invokeDesktop("desktop_trigger_delete", {
        request,
      }),
    deleteBackendConfig: (request) =>
      invokeDesktop("desktop_backend_delete", {
        request,
      }),
    deleteInferenceProfileConfig: (request) =>
      invokeDesktop("desktop_inference_profile_delete", {
        request,
      }),
    deleteToolsConfig: (request) =>
      invokeDesktop("desktop_tools_delete", {
        request,
      }),
    deleteToolServiceConfig: (request) =>
      invokeDesktop("desktop_tool_service_delete", {
        request,
      }),
    deleteAgentConfig: (request) =>
      invokeDesktop("desktop_agent_delete", {
        request,
      }),
    deleteContextConfig: (request) =>
      invokeDesktop("desktop_context_delete", {
        request,
      }),
    saveBackendConfig: (request) =>
      invokeDesktop("desktop_backend_save", { request }),
    probeInferenceEndpoint: (endpoint) =>
      invokeDesktop("desktop_probe_inference_endpoint", {
        request: { endpoint },
      }),
    getInferenceSetupCatalog: () =>
      invokeDesktop("desktop_inference_setup_catalog"),
    discoverInferenceModels: (request) =>
      invokeDesktop("desktop_inference_models_discover", {
        request,
      }),
    getInferenceModelRecommendation: (request) =>
      invokeDesktop("desktop_inference_model_recommendation", { request }),
    getInferenceBackendRecommendation: (request) =>
      invokeDesktop("desktop_inference_backend_recommendation", { request }),
    codexLogin: (nodeDid, provider, label) =>
      invokeDesktop("desktop_codex_login", {
        request: { nodeDid, provider: provider ?? null, label: label ?? null },
      }),
    cancelCodexLogin: () => invokeDesktop("desktop_codex_login_cancel"),
    grokLogin: (nodeDid, provider, label) =>
      invokeDesktop("desktop_grok_login", {
        request: { nodeDid, provider: provider ?? null, label: label ?? null },
      }),
    cancelGrokLogin: () => invokeDesktop("desktop_grok_login_cancel"),
    claudeLogin: (nodeDid, provider, label) =>
      invokeDesktop("desktop_claude_login", {
        request: { nodeDid, provider: provider ?? null, label: label ?? null },
      }),
    cancelClaudeLogin: () => invokeDesktop("desktop_claude_login_cancel"),
    watchProviderLoginUrl: (provider, onUrl) =>
      transport.listenProviderLoginUrl?.(provider, onUrl) ??
      Promise.resolve(() => {}),
    ...createProviderAccountCommands(invokeDesktop),
    ...createPackCommands(invokeDesktop),
    ...createHostAccessCommands(invokeDesktop),
    ...createUxPluginCommands(invokeDesktop),
    saveInferenceProfileConfig: (request) =>
      invokeDesktop("desktop_inference_profile_save", {
        request,
      }),
    saveToolsConfig: (request) =>
      invokeDesktop("desktop_tools_save", {
        request,
      }),
    saveToolServiceConfig: (request) =>
      invokeDesktop("desktop_tool_service_save", {
        request,
      }),
    testToolService: (request) =>
      invokeDesktop("desktop_tool_service_test", {
        request,
      }),
    saveTaskConfig: (request) =>
      invokeDesktop("desktop_task_save", { request }),
    saveScheduleConfig: (request) =>
      invokeDesktop("desktop_schedule_save", {
        request,
      }),
    runSchedule: (request) =>
      invokeDesktop("desktop_schedule_run", { request }),
    saveTriggerConfig: (request) =>
      invokeDesktop("desktop_trigger_save", {
        request,
      }),
    runTask: (request) => invokeDesktop("desktop_task_run", { request }),
    sessionProvenance: (request) =>
      invokeDesktop("desktop_session_provenance", {
        request,
      }),
    listBackendsWithHealth: () =>
      invokeDesktop("desktop_list_backends_with_health"),
    listMcpServicesWithHealth: () =>
      invokeDesktop("desktop_list_mcp_services_with_health"),
    probeMcpService: (nodeDid, serviceId) =>
      invokeDesktop("desktop_probe_mcp_service", {
        request: { nodeDid, serviceId },
      }),
    fetchOperationsSnapshot: (request) =>
      invokeDesktop("desktop_operations_snapshot", {
        request,
      }),
    interruptRequest: (request) =>
      invokeDesktop("desktop_interrupt_request", {
        request,
      }),
    backgroundTool: (request) =>
      invokeDesktop("desktop_background_tool", { request }),
    jobControl: (request) => invokeDesktop("desktop_job_control", { request }),
  };
}
