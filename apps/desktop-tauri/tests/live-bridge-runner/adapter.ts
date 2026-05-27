import type { DesktopApiAdapter } from "../../src/lib/desktop-api";
import type { BackendHealth } from "../../src/components/backendHealth/types";
import type {
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
} from "../../src/lib/types";
import type { TauriDriverChatRequest } from "../tauri-driver";
import type { LiveBridgeRunner } from "../live-bridge-runner";
import { normalizePeerStatusUrl } from "./process";

export function createRunnerAdapter(runner: LiveBridgeRunner): DesktopApiAdapter {
  return {
    fetchDesktopSnapshot: async () =>
      runner.getJson<DesktopClientSnapshot>("/desktop/client/snapshot"),
    initLocalStandardRuntime: async () =>
      runner.postJson<InitSummary>("/desktop/init", {}),
    startDesktopClient: async () =>
      runner.postJson<DesktopClientSnapshot>("/desktop/client/start", {}),
    shutdownDesktopClient: async () =>
      runner.postJson<DesktopClientSnapshot>("/desktop/client/shutdown", {}),
    setSelectedAgent: async (agentDid) => {
      await runner.postJson("/desktop/selected-agent", { agentDid });
    },
    addPeer: async (request) =>
      runner.postJson<DesktopClientSnapshot>("/desktop/peer/add", request),
    fetchPeerStatus: async (serverAddress) => {
      const response = await runner.fetchWithTimeout(
        normalizePeerStatusUrl(serverAddress),
        {},
      );
      return runner.decodeJson<unknown>(response);
    },
    repairP2P: async () =>
      runner.postJson<DesktopClientSnapshot>("/desktop/p2p/repair", {}),
    fetchSessionSnapshot: async (sessionId, agentDid, requestId) =>
      runner.postJson<DesktopSessionSnapshot | null>("/desktop/session/snapshot", {
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
      runner.sentRequests.push(normalized);
      const result = await runner.postJson<ChatSendResult>(
        "/desktop/chat/send",
        normalized,
      );
      runner.sendResults.push(result);
      return result;
    },
    renameConversation: async (request) => {
      await runner.postJson("/desktop/conversation/rename", request);
    },
    saveAgentConfig: async (request) =>
      runner.postJson<DesktopClientSnapshot>("/desktop/agent/save", request),
    saveBehaviorConfig: async (request) =>
      runner.postJson<DesktopClientSnapshot>("/desktop/behavior/save", request),
    saveBackendConfig: async (request) =>
      runner.postJson<DesktopClientSnapshot>("/desktop/backend/save", request),
    saveInferenceProfileConfig: async (request) =>
      runner.postJson<DesktopClientSnapshot>(
        "/desktop/inference-profile/save",
        request,
      ),
    saveToolSelectionConfig: async (request) =>
      runner.postJson<DesktopClientSnapshot>("/desktop/tool-selection/save", request),
    saveToolServiceConfig: async (request) =>
      runner.postJson<DesktopClientSnapshot>("/desktop/tool-service/save", request),
    testToolService: async (request) =>
      runner.postJson("/desktop/tool-service/test", request),
    saveTaskConfig: async (request) =>
      runner.postJson<DesktopClientSnapshot>("/desktop/task/save", request),
    saveScheduleConfig: async (request) =>
      runner.postJson<DesktopClientSnapshot>("/desktop/schedule/save", request),
    runSchedule: async (request) => {
      const result = await runner.postJson<TaskRunResult>(
        "/desktop/schedule/run",
        request,
      );
      runner.taskRunResults.push(result);
      return result;
    },
    saveEventTriggerConfig: async (request) =>
      runner.postJson<DesktopClientSnapshot>("/desktop/event-trigger/save", request),
    runTask: async (request) => {
      const result = await runner.postJson<TaskRunResult>("/desktop/task/run", request);
      runner.taskRunResults.push(result);
      return result;
    },
    listSubagentTree: async (request) =>
      runner.postJson<SubagentTreeView>("/desktop/subagent-tree", request),
    listBackendsWithHealth: async () =>
      runner.getJson<BackendHealth[]>("/desktop/backend-health"),
    listMcpServicesWithHealth: async () =>
      runner.getJson<MCPServiceHealthView[]>("/desktop/mcp-health"),
    probeMcpService: async (serviceId) =>
      runner.postJson<McpServiceProbeResult>("/desktop/mcp/probe", { serviceId }),
    fetchOperationsSnapshot: async (request) =>
      runner.postJson<DesktopOperationsSnapshot>(
        "/desktop/operations/snapshot",
        request,
      ),
    previewInterruptCascade: async (request) =>
      runner.postJson<CascadeCancelPreview>("/desktop/interrupt/preview", request),
    interruptRequest: async (request) =>
      runner.postJson<InterruptRequestResult>("/desktop/interrupt/request", request),
  };
}

/**
 * Test-only fixture helpers that are NOT part of DesktopApiAdapter (production
 * interface).  Kept here, close to the runner, so they never bleed into the
 * Tauri bridge or the browser bundle.
 */
export function createFixtureHelpers(runner: {
  postJson: <T>(path: string, body: unknown) => Promise<T>;
}) {
  return {
    /** Write a behavior document on the *remote* node.  The write triggers P2P
     *  replication so the same document becomes visible on the desktop node.
     *  This is the D1/D2 cross-node witness — write-on-A, read-on-B.
     *  Requires DEFRA_AGENT_TAURI_LIVE=1 (enforced server-side). */
    saveBehaviorConfigOnRemote: async (request: BehaviorSaveRequest) =>
      runner.postJson<{ ok: boolean }>(
        "/desktop/test-fixture/remote-save-behavior",
        request,
      ),
  };
}
