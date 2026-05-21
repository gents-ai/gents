import type { DesktopApiAdapter } from "../../src/lib/desktop-api";
import type {
  CascadeCancelPreview,
  ChatSendResult,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  InitSummary,
  InterruptRequestResult,
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
    previewInterruptCascade: async (request) =>
      runner.postJson<CascadeCancelPreview>("/desktop/interrupt/preview", request),
    interruptRequest: async (request) =>
      runner.postJson<InterruptRequestResult>("/desktop/interrupt/request", request),
  };
}
