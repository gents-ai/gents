import type { Dispatch, SetStateAction } from "react";

import type {
  AgentConfigSaveRequest,
  DefaultBehaviorSetRequest,
  BackendSaveRequest,
  ConfigComponentsPatchRequest,
  ConfigComponentsApplyRequest,
  ContextDeleteRequest,
  BehaviorSaveRequest,
  CodexLoginResult,
  DesktopApiAdapter,
  InferenceProbeResult,
  InferenceProfileSaveRequest,
  SkillDeleteRequest,
  SkillSaveRequest,
  ToolsSaveRequest,
  ToolServiceSaveRequest,
  ToolServiceTestRequest,
  ToolServiceTestResult,
  TaskDeleteRequest,
  ScheduleDeleteRequest,
  EventSourceDeleteRequest,
  TriggerDeleteRequest,
  BackendDeleteRequest,
  InferenceProfileDeleteRequest,
  ToolsDeleteRequest,
  ToolServiceDeleteRequest,
  BehaviorDeleteRequest,
} from "@source-inc/gents-desktop-client";

import { actionFailure, shownFailure } from "./desktopShellRuntime";

type ConfigActionParams = {
  api: DesktopApiAdapter;
  setError: Dispatch<SetStateAction<string | null>>;
  mutateSnapshot: <T>(operation: () => Promise<T>) => Promise<T>;
};

export function createDesktopShellConfigActions({
  api,
  setError,
  mutateSnapshot,
}: ConfigActionParams) {
  /** A configuration change: a fresh read once it lands, and one report if
      it fails, naming what failed. The caller still sees the failure, to keep
      what the person typed. */
  async function change<T>(label: string, run: () => Promise<T>) {
    setError(null);
    try {
      return await mutateSnapshot(run);
    } catch (error) {
      setError(actionFailure(label, error));
      throw shownFailure(error);
    }
  }

  /** A call that stores nothing: reported the same way, without a re-read. */
  async function call<T>(label: string, run: () => Promise<T>) {
    setError(null);
    try {
      return await run();
    } catch (error) {
      setError(actionFailure(label, error));
      throw shownFailure(error);
    }
  }

  return {
    onSaveAgentConfig: (request: AgentConfigSaveRequest) =>
      change("save the agent", () => api.saveAgentConfig(request)),
    onSetDefaultBehavior: (request: DefaultBehaviorSetRequest) =>
      change("set the default behavior", () => api.setDefaultBehavior(request)),
    onSaveBehaviorConfig: (request: BehaviorSaveRequest) =>
      change("save the behavior", () => api.saveBehaviorConfig(request)),
    onSaveSkillConfig: (request: SkillSaveRequest) =>
      change("save the skill", () => api.saveSkillConfig(request)),
    onDeleteSkillConfig: (request: SkillDeleteRequest) =>
      change("delete the skill", () => api.deleteSkillConfig(request)),
    onDeleteContextConfig: (request: ContextDeleteRequest) =>
      change("delete the context", () => api.deleteContextConfig(request)),
    onDeleteTaskConfig: (request: TaskDeleteRequest) =>
      change("delete the task", () => api.deleteTaskConfig(request)),
    onDeleteScheduleConfig: (request: ScheduleDeleteRequest) =>
      change("delete the schedule", () => api.deleteScheduleConfig(request)),
    onDeleteEventSourceConfig: (request: EventSourceDeleteRequest) =>
      change("delete the event source", () => api.deleteEventSourceConfig(request)),
    onDeleteTriggerConfig: (request: TriggerDeleteRequest) =>
      change("delete the trigger", () => api.deleteTriggerConfig(request)),
    onDeleteBackendConfig: (request: BackendDeleteRequest) =>
      change("delete the backend", () => api.deleteBackendConfig(request)),
    onDeleteInferenceProfileConfig: (request: InferenceProfileDeleteRequest) =>
      change("delete the inference profile", () =>
        api.deleteInferenceProfileConfig(request),
      ),
    onDeleteToolsConfig: (request: ToolsDeleteRequest) =>
      change("delete the tools", () => api.deleteToolsConfig(request)),
    onDeleteToolServiceConfig: (request: ToolServiceDeleteRequest) =>
      change("delete the tool service", () => api.deleteToolServiceConfig(request)),
    onDeleteBehaviorConfig: (request: BehaviorDeleteRequest) =>
      change("delete the behavior", () => api.deleteBehaviorConfig(request)),
    onSaveBackendConfig: (request: BackendSaveRequest) =>
      change("save the backend", () => api.saveBackendConfig(request)),
    onPatchConfigComponents: (request: ConfigComponentsPatchRequest) =>
      change("save the configuration", () => api.patchConfigComponents(request)),
    onApplyConfigComponents: (request: ConfigComponentsApplyRequest) =>
      change("apply the configuration", () => api.applyConfigComponents(request)),
    onSaveInferenceProfileConfig: (request: InferenceProfileSaveRequest) =>
      change("save the inference profile", () =>
        api.saveInferenceProfileConfig(request),
      ),
    onSaveToolsConfig: (request: ToolsSaveRequest) =>
      change("save the tools", () => api.saveToolsConfig(request)),
    onSaveToolServiceConfig: (request: ToolServiceSaveRequest) =>
      change("save the tool service", () => api.saveToolServiceConfig(request)),
    onProbeInferenceEndpoint: (endpoint: string): Promise<InferenceProbeResult> =>
      api.probeInferenceEndpoint(endpoint),
    onCodexLogin: (agentDid: string): Promise<CodexLoginResult> =>
      call("sign in to Codex", () => api.codexLogin(agentDid)),
    /* best-effort abort of a sign-in whose browser was closed; a failure here
       (nothing in flight, say) must never block closing the wizard */
    onCancelCodexLogin: (): Promise<void> => api.cancelCodexLogin().catch(() => {}),
    onGrokLogin: (agentDid: string) =>
      call("sign in to Grok", () => api.grokLogin(agentDid)),
    onCancelGrokLogin: (): Promise<void> => api.cancelGrokLogin().catch(() => {}),
    onTestToolService: (
      request: ToolServiceTestRequest,
    ): Promise<ToolServiceTestResult> =>
      call("test the tool service", () => api.testToolService(request)),
  };
}
