import type {
  AgentConfigSaveRequest,
  BackendDeleteRequest,
  BackendSaveRequest,
  BehaviorDeleteRequest,
  BehaviorSaveRequest,
  EventTriggerDeleteRequest,
  EventTriggerSaveRequest,
  InferenceProfileDeleteRequest,
  InferenceProfileSaveRequest,
  ScheduleDeleteRequest,
  ScheduleRunRequest,
  ScheduleSaveRequest,
  SkillDeleteRequest,
  SkillSaveRequest,
  TaskDeleteRequest,
  TaskRunRequest,
  TaskSaveRequest,
  ToolSelectionDeleteRequest,
  ToolSelectionSaveRequest,
  ToolServiceDeleteRequest,
  ToolServiceSaveRequest,
  ToolServiceTestRequest,
} from "../types.js";
import { getDesktopApiAdapter } from "./adapter.js";
import type { DesktopApiAdapter } from "./types.js";

export function explainToolSurface(
  agentDid: string,
  behaviorId: string,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).explainToolSurface(agentDid, behaviorId);
}

export function saveAgentConfig(
  request: AgentConfigSaveRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).saveAgentConfig(request);
}

export function saveBehaviorConfig(
  request: BehaviorSaveRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).saveBehaviorConfig(request);
}

export function saveSkillConfig(
  request: SkillSaveRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).saveSkillConfig(request);
}

export function deleteSkillConfig(
  request: SkillDeleteRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).deleteSkillConfig(request);
}

export function deleteTaskConfig(
  request: TaskDeleteRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).deleteTaskConfig(request);
}

export function deleteScheduleConfig(
  request: ScheduleDeleteRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).deleteScheduleConfig(request);
}

export function deleteEventTriggerConfig(
  request: EventTriggerDeleteRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).deleteEventTriggerConfig(request);
}

export function deleteBackendConfig(
  request: BackendDeleteRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).deleteBackendConfig(request);
}

export function deleteInferenceProfileConfig(
  request: InferenceProfileDeleteRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).deleteInferenceProfileConfig(request);
}

export function deleteToolSelectionConfig(
  request: ToolSelectionDeleteRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).deleteToolSelectionConfig(request);
}

export function deleteToolServiceConfig(
  request: ToolServiceDeleteRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).deleteToolServiceConfig(request);
}

export function deleteBehaviorConfig(
  request: BehaviorDeleteRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).deleteBehaviorConfig(request);
}

export function saveBackendConfig(
  request: BackendSaveRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).saveBackendConfig(request);
}

export function probeInferenceEndpoint(
  endpoint: string,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).probeInferenceEndpoint(endpoint);
}

export function codexLogin(
  agentDid: string,
  provider?: string | null,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).codexLogin(agentDid, provider);
}

export function cancelCodexLogin(api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).cancelCodexLogin();
}

export function grokLogin(
  agentDid: string,
  provider?: string | null,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).grokLogin(agentDid, provider);
}

export function cancelGrokLogin(api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).cancelGrokLogin();
}

export function listProviderAccounts(
  agentDid: string,
  api?: DesktopApiAdapter,
) {
  const action = getDesktopApiAdapter(api).listProviderAccounts;
  if (!action)
    throw new Error("Provider accounts are not supported by this build");
  return action(agentDid);
}

export function disconnectProviderAccount(
  agentDid: string,
  credentialId: string,
  api?: DesktopApiAdapter,
) {
  const action = getDesktopApiAdapter(api).disconnectProviderAccount;
  if (!action)
    throw new Error("Provider accounts are not supported by this build");
  return action(agentDid, credentialId);
}

export function saveInferenceProfileConfig(
  request: InferenceProfileSaveRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).saveInferenceProfileConfig(request);
}

export function saveToolSelectionConfig(
  request: ToolSelectionSaveRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).saveToolSelectionConfig(request);
}

export function saveToolServiceConfig(
  request: ToolServiceSaveRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).saveToolServiceConfig(request);
}

export function testToolService(
  request: ToolServiceTestRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).testToolService(request);
}

export function saveTaskConfig(
  request: TaskSaveRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).saveTaskConfig(request);
}

export function saveScheduleConfig(
  request: ScheduleSaveRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).saveScheduleConfig(request);
}

export function runSchedule(
  request: ScheduleRunRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).runSchedule(request);
}

export function saveEventTriggerConfig(
  request: EventTriggerSaveRequest,
  api?: DesktopApiAdapter,
) {
  return getDesktopApiAdapter(api).saveEventTriggerConfig(request);
}

export function runTask(request: TaskRunRequest, api?: DesktopApiAdapter) {
  return getDesktopApiAdapter(api).runTask(request);
}
