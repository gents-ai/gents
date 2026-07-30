import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import type { BackendHealth } from "@source-inc/gents-desktop-client";
import type {
  BearerPairingResponse,
  BehaviorSaveRequest,
  CascadeCancelPreview,
  ChatSendResult,
  DesktopClientSnapshot,
  DesktopOperationsSnapshot,
  DesktopSessionSnapshot,
  InitSummary,
  InterruptRequestResult,
  MCPServiceHealthView,
  McpServiceProbeResult,
  SubagentTreeView,
  TaskRunResult,
} from "@source-inc/gents-desktop-client";
import type { TauriDriverChatRequest } from "../tauri-driver";
import type { LiveBridgeRunner } from "../live-bridge-runner";

type BridgeHttpClient = {
  getJson: <T>(path: string) => Promise<T>;
  postJson: <T = unknown>(path: string, body: unknown) => Promise<T>;
};

type BridgeAdapterObservers = {
  onChatRequest?: (request: TauriDriverChatRequest) => void;
  onChatResult?: (result: ChatSendResult) => void;
  onTaskRunResult?: (result: TaskRunResult) => void;
};

export function createRunnerAdapter(runner: LiveBridgeRunner): DesktopApiAdapter {
  return createBridgeHttpAdapter(runner, {
    onChatRequest: (request) => runner.sentRequests.push(request),
    onChatResult: (result) => runner.sendResults.push(result),
    onTaskRunResult: (result) => runner.taskRunResults.push(result),
  });
}

export function createBridgeHttpAdapter(
  client: BridgeHttpClient,
  observers: BridgeAdapterObservers = {},
): DesktopApiAdapter {
  return {
    fetchDesktopSnapshot: async () =>
      client.getJson<DesktopClientSnapshot>("/desktop/client/snapshot"),
    initLocalStandardRuntime: async () =>
      client.postJson<InitSummary>("/desktop/init", {}),
    startDesktopClient: async () =>
      client.postJson<DesktopClientSnapshot>("/desktop/client/start", {}),
    shutdownDesktopClient: async () =>
      client.postJson<DesktopClientSnapshot>("/desktop/client/shutdown", {}),
    setSelectedAgent: async (agentDid) => {
      await client.postJson("/desktop/selected-agent", { agentDid });
    },
    addPeer: async (request) =>
      client.postJson<DesktopClientSnapshot>("/desktop/peer/add", request),
    pairBearer: async (request) =>
      client.postJson<BearerPairingResponse>("/desktop/peer/pair-bearer", request),
    fetchPeerStatus: async (peerId) =>
      client.postJson("/desktop/peer/status", { peerId }),
    probePeerAddress: async (serverAddress) =>
      client.postJson("/desktop/peer/status", { serverAddress }),
    repairP2P: async () =>
      client.postJson<DesktopClientSnapshot>("/desktop/p2p/repair", {}),
    fetchSessionSnapshot: async (sessionId, agentDid, requestId) =>
      client.postJson<DesktopSessionSnapshot | null>("/desktop/session/snapshot", {
        sessionId,
        agentDid: agentDid ?? null,
        requestId: requestId ?? null,
      }),
    sendChatMessage: async (request) => {
      const normalized: TauriDriverChatRequest = {
        agentDid: request.agentDid,
        behaviorId: request.behaviorId ?? null,
        sessionId: request.sessionId ?? null,
        content: request.content,
      };
      observers.onChatRequest?.(normalized);
      const result = await client.postJson<ChatSendResult>(
        "/desktop/chat/send",
        normalized,
      );
      observers.onChatResult?.(result);
      return result;
    },
    renameConversation: async (request) => {
      await client.postJson("/desktop/conversation/rename", request);
    },
    saveAgentConfig: async (request) =>
      client.postJson<DesktopClientSnapshot>("/desktop/agent/save", request),
    saveBehaviorConfig: async (request) =>
      client.postJson<DesktopClientSnapshot>("/desktop/behavior/save", request),
    saveBackendConfig: async (request) =>
      client.postJson<DesktopClientSnapshot>("/desktop/backend/save", request),
    saveInferenceProfileConfig: async (request) =>
      client.postJson<DesktopClientSnapshot>(
        "/desktop/inference-profile/save",
        request,
      ),
    saveToolSelectionConfig: async (request) =>
      client.postJson<DesktopClientSnapshot>("/desktop/tool-selection/save", request),
    saveToolServiceConfig: async (request) =>
      client.postJson<DesktopClientSnapshot>("/desktop/tool-service/save", request),
    testToolService: async (request) =>
      client.postJson("/desktop/tool-service/test", request),
    saveTaskConfig: async (request) =>
      client.postJson<DesktopClientSnapshot>("/desktop/task/save", request),
    saveScheduleConfig: async (request) =>
      client.postJson<DesktopClientSnapshot>("/desktop/schedule/save", request),
    runSchedule: async (request) => {
      const result = await client.postJson<TaskRunResult>(
        "/desktop/schedule/run",
        request,
      );
      observers.onTaskRunResult?.(result);
      return result;
    },
    saveEventTriggerConfig: async (request) =>
      client.postJson<DesktopClientSnapshot>("/desktop/event-trigger/save", request),
    runTask: async (request) => {
      const result = await client.postJson<TaskRunResult>("/desktop/task/run", request);
      observers.onTaskRunResult?.(result);
      return result;
    },
    listSubagentTree: async (request) =>
      client.postJson<SubagentTreeView>("/desktop/subagent-tree", request),
    listBackendsWithHealth: async () =>
      client.getJson<BackendHealth[]>("/desktop/backend-health"),
    listMcpServicesWithHealth: async () =>
      client.getJson<MCPServiceHealthView[]>("/desktop/mcp-health"),
    probeMcpService: async (serviceId) =>
      client.postJson<McpServiceProbeResult>("/desktop/mcp/probe", { serviceId }),
    fetchOperationsSnapshot: async (request) =>
      client.postJson<DesktopOperationsSnapshot>(
        "/desktop/operations/snapshot",
        request,
      ),
    previewInterruptCascade: async (request) =>
      client.postJson<CascadeCancelPreview>("/desktop/interrupt/preview", request),
    interruptRequest: async (request) =>
      client.postJson<InterruptRequestResult>("/desktop/interrupt/request", request),
  };
}

export function createFixtureHelpers(runner: {
  postJson: <T>(path: string, body: unknown) => Promise<T>;
}) {
  return {
    /** Write a behavior document on the *remote* node.  The write triggers P2P
     *  replication so the same document becomes visible on the desktop node.
     *  This is the D1/D2 cross-node witness — write-on-A, read-on-B.
     *  Requires GENTS_TAURI_LIVE=1 (enforced server-side). */
    saveBehaviorConfigOnRemote: async (request: BehaviorSaveRequest) =>
      runner.postJson<{ ok: boolean }>(
        "/desktop/test-fixture/remote-save-behavior",
        request,
      ),
  };
}
