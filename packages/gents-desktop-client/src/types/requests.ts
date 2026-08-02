import type { AgentConfigSaveRequest as GeneratedAgentConfigSaveRequest } from "../generated/AgentConfigSaveRequest.js";
import type { BackendDeleteRequest as GeneratedBackendDeleteRequest } from "../generated/BackendDeleteRequest.js";
import type { BackendSaveRequest as GeneratedBackendSaveRequest } from "../generated/BackendSaveRequest.js";
import type { BearerPairingRequest as GeneratedBearerPairingRequest } from "../generated/BearerPairingRequest.js";
import type { BehaviorDeleteRequest as GeneratedBehaviorDeleteRequest } from "../generated/BehaviorDeleteRequest.js";
import type { BehaviorSaveRequest as GeneratedBehaviorSaveRequest } from "../generated/BehaviorSaveRequest.js";
import type { ChatSendRequest as GeneratedChatSendRequest } from "../generated/ChatSendRequest.js";
import type { ConversationRenameRequest as GeneratedConversationRenameRequest } from "../generated/ConversationRenameRequest.js";
import type { DesktopInitRequest as GeneratedDesktopInitRequest } from "../generated/DesktopInitRequest.js";
import type { EventTriggerDeleteRequest as GeneratedEventTriggerDeleteRequest } from "../generated/EventTriggerDeleteRequest.js";
import type { EventTriggerSaveRequest as GeneratedEventTriggerSaveRequest } from "../generated/EventTriggerSaveRequest.js";
import type { InferenceProfileDeleteRequest as GeneratedInferenceProfileDeleteRequest } from "../generated/InferenceProfileDeleteRequest.js";
import type { InferenceProfileSaveRequest as GeneratedInferenceProfileSaveRequest } from "../generated/InferenceProfileSaveRequest.js";
import type { InferenceProbeRequest as GeneratedInferenceProbeRequest } from "../generated/InferenceProbeRequest.js";
import type { CodexLoginRequest as GeneratedCodexLoginRequest } from "../generated/CodexLoginRequest.js";
import type { PeerAddRequest as GeneratedPeerAddRequest } from "../generated/PeerAddRequest.js";
import type { PeerProbeRequest as GeneratedPeerProbeRequest } from "../generated/PeerProbeRequest.js";
import type { PeerStatusFetchRequest as GeneratedPeerStatusFetchRequest } from "../generated/PeerStatusFetchRequest.js";
import type { ScheduleDeleteRequest as GeneratedScheduleDeleteRequest } from "../generated/ScheduleDeleteRequest.js";
import type { ScheduleRunRequest as GeneratedScheduleRunRequest } from "../generated/ScheduleRunRequest.js";
import type { ScheduleSaveRequest as GeneratedScheduleSaveRequest } from "../generated/ScheduleSaveRequest.js";
import type { SkillDeleteRequest as GeneratedSkillDeleteRequest } from "../generated/SkillDeleteRequest.js";
import type { SkillSaveRequest as GeneratedSkillSaveRequest } from "../generated/SkillSaveRequest.js";
import type { TaskDeleteRequest as GeneratedTaskDeleteRequest } from "../generated/TaskDeleteRequest.js";
import type { TaskRunRequest as GeneratedTaskRunRequest } from "../generated/TaskRunRequest.js";
import type { TaskSaveRequest as GeneratedTaskSaveRequest } from "../generated/TaskSaveRequest.js";
import type { ToolSelectionDeleteRequest as GeneratedToolSelectionDeleteRequest } from "../generated/ToolSelectionDeleteRequest.js";
import type { ToolSelectionSaveRequest as GeneratedToolSelectionSaveRequest } from "../generated/ToolSelectionSaveRequest.js";
import type { ToolServiceDeleteRequest as GeneratedToolServiceDeleteRequest } from "../generated/ToolServiceDeleteRequest.js";
import type { ToolServiceSaveRequest as GeneratedToolServiceSaveRequest } from "../generated/ToolServiceSaveRequest.js";
import type { ToolServiceTestRequest as GeneratedToolServiceTestRequest } from "../generated/ToolServiceTestRequest.js";

type RequestInput<T> = {
  [K in keyof T as null extends T[K] ? never : K]: T[K];
} & {
  [K in keyof T as null extends T[K] ? K : never]?: T[K];
};

export type AgentConfigSaveRequest =
  RequestInput<GeneratedAgentConfigSaveRequest>;
export type BackendDeleteRequest = RequestInput<GeneratedBackendDeleteRequest>;
export type BackendSaveRequest = RequestInput<GeneratedBackendSaveRequest>;
export type BearerPairingRequest = RequestInput<GeneratedBearerPairingRequest>;
export type BehaviorDeleteRequest =
  RequestInput<GeneratedBehaviorDeleteRequest>;
export type BehaviorSaveRequest = RequestInput<GeneratedBehaviorSaveRequest>;
export type ChatSendRequest = RequestInput<GeneratedChatSendRequest>;
export type ConversationRenameRequest =
  RequestInput<GeneratedConversationRenameRequest>;
export type DesktopInitRequest = RequestInput<GeneratedDesktopInitRequest>;
export type EventTriggerDeleteRequest =
  RequestInput<GeneratedEventTriggerDeleteRequest>;
export type EventTriggerSaveRequest =
  RequestInput<GeneratedEventTriggerSaveRequest>;
export type InferenceProfileDeleteRequest =
  RequestInput<GeneratedInferenceProfileDeleteRequest>;
export type InferenceProfileSaveRequest =
  RequestInput<GeneratedInferenceProfileSaveRequest>;
export type InferenceProbeRequest =
  RequestInput<GeneratedInferenceProbeRequest>;
export type CodexLoginRequest = RequestInput<GeneratedCodexLoginRequest>;
export type PeerAddRequest = RequestInput<GeneratedPeerAddRequest>;
export type PeerProbeRequest = RequestInput<GeneratedPeerProbeRequest>;
export type PeerStatusFetchRequest =
  RequestInput<GeneratedPeerStatusFetchRequest>;
export type ScheduleDeleteRequest =
  RequestInput<GeneratedScheduleDeleteRequest>;
export type ScheduleRunRequest = RequestInput<GeneratedScheduleRunRequest>;
export type ScheduleSaveRequest = RequestInput<GeneratedScheduleSaveRequest>;
export type SkillDeleteRequest = RequestInput<GeneratedSkillDeleteRequest>;
export type SkillSaveRequest = RequestInput<GeneratedSkillSaveRequest>;
export type TaskDeleteRequest = RequestInput<GeneratedTaskDeleteRequest>;
export type TaskRunRequest = RequestInput<GeneratedTaskRunRequest>;
export type TaskSaveRequest = RequestInput<GeneratedTaskSaveRequest>;
export type ToolSelectionDeleteRequest =
  RequestInput<GeneratedToolSelectionDeleteRequest>;
export type ToolSelectionSaveRequest =
  RequestInput<GeneratedToolSelectionSaveRequest>;
export type ToolServiceDeleteRequest =
  RequestInput<GeneratedToolServiceDeleteRequest>;
export type ToolServiceSaveRequest =
  RequestInput<GeneratedToolServiceSaveRequest>;
export type ToolServiceTestRequest =
  RequestInput<GeneratedToolServiceTestRequest>;

export type { TaskRunResult } from "../generated/TaskRunResult.js";
export type { ToolServiceTestResult } from "../generated/ToolServiceTestResult.js";
export type { ToolServiceToolView } from "../generated/ToolServiceToolView.js";
export type { CodexLoginResult } from "../generated/CodexLoginResult.js";
export type { GrokLoginResult } from "../generated/GrokLoginResult.js";
export type { GrokLoginRequest } from "../generated/GrokLoginRequest.js";
export type { GrokLoginUrl } from "../generated/GrokLoginUrl.js";
export type { CodexLoginUrl } from "../generated/CodexLoginUrl.js";
export type { InferenceProbeResult } from "../generated/InferenceProbeResult.js";
