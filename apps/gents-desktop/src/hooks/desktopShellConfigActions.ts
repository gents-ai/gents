import type { Dispatch, SetStateAction } from "react";

import type {
  AgentConfigSaveRequest,
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

type ConfigActionParams = {
  api: DesktopApiAdapter;
  setError: Dispatch<SetStateAction<string | null>>;
  setSavingBehaviorConfig: Dispatch<SetStateAction<boolean>>;
  setSavingConfig: Dispatch<SetStateAction<boolean>>;
  mutateSnapshot: <T>(operation: () => Promise<T>) => Promise<T>;
};

export function createDesktopShellConfigActions({
  api,
  setError,
  setSavingBehaviorConfig,
  setSavingConfig,
  mutateSnapshot,
}: ConfigActionParams) {
  async function onSaveAgentConfig(request: AgentConfigSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.saveAgentConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onSaveBehaviorConfig(request: BehaviorSaveRequest) {
    setSavingBehaviorConfig(true);
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.saveBehaviorConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingBehaviorConfig(false);
      setSavingConfig(false);
    }
  }

  async function onSaveSkillConfig(request: SkillSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.saveSkillConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onDeleteSkillConfig(request: SkillDeleteRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.deleteSkillConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onDeleteContextConfig(request: ContextDeleteRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      return await mutateSnapshot(() => api.deleteContextConfig(request));
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onDeleteTaskConfig(request: TaskDeleteRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.deleteTaskConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onDeleteScheduleConfig(request: ScheduleDeleteRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.deleteScheduleConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onDeleteEventSourceConfig(request: EventSourceDeleteRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.deleteEventSourceConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onDeleteTriggerConfig(request: TriggerDeleteRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.deleteTriggerConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onDeleteBackendConfig(request: BackendDeleteRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.deleteBackendConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onDeleteInferenceProfileConfig(
    request: InferenceProfileDeleteRequest,
  ) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() =>
        api.deleteInferenceProfileConfig(request),
      );
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onDeleteToolsConfig(request: ToolsDeleteRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.deleteToolsConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onDeleteToolServiceConfig(request: ToolServiceDeleteRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.deleteToolServiceConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onDeleteBehaviorConfig(request: BehaviorDeleteRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.deleteBehaviorConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onSaveBackendConfig(request: BackendSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.saveBackendConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onPatchConfigComponents(request: ConfigComponentsPatchRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.patchConfigComponents(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onApplyConfigComponents(request: ConfigComponentsApplyRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.applyConfigComponents(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onProbeInferenceEndpoint(
    endpoint: string,
  ): Promise<InferenceProbeResult> {
    return api.probeInferenceEndpoint(endpoint);
  }

  async function onCodexLogin(agentDid: string): Promise<CodexLoginResult> {
    setError(null);
    try {
      return await api.codexLogin(agentDid);
    } catch (err) {
      setError(String(err));
      throw err;
    }
  }

  async function onCancelCodexLogin(): Promise<void> {
    // Best-effort abort of a sign-in whose browser was closed; a failure here
    // (e.g. nothing in flight) must never block closing the wizard.
    try {
      await api.cancelCodexLogin();
    } catch {}
  }

  async function onGrokLogin(agentDid: string) {
    setError(null);
    try {
      return await api.grokLogin(agentDid);
    } catch (err) {
      setError(String(err));
      throw err;
    }
  }

  async function onCancelGrokLogin(): Promise<void> {
    try {
      await api.cancelGrokLogin();
    } catch {}
  }

  async function onSaveInferenceProfileConfig(request: InferenceProfileSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.saveInferenceProfileConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onSaveToolsConfig(request: ToolsSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.saveToolsConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onSaveToolServiceConfig(request: ToolServiceSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await mutateSnapshot(() => api.saveToolServiceConfig(request));
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onTestToolService(
    request: ToolServiceTestRequest,
  ): Promise<ToolServiceTestResult> {
    setError(null);
    try {
      return await api.testToolService(request);
    } catch (err) {
      setError(String(err));
      throw err;
    }
  }

  return {
    onSaveAgentConfig,
    onSaveBackendConfig,
    onPatchConfigComponents,
    onApplyConfigComponents,
    onSaveBehaviorConfig,
    onDeleteSkillConfig,
    onDeleteContextConfig,
    onDeleteTaskConfig,
    onDeleteScheduleConfig,
    onDeleteEventSourceConfig,
    onDeleteTriggerConfig,
    onDeleteBackendConfig,
    onDeleteInferenceProfileConfig,
    onDeleteToolsConfig,
    onDeleteToolServiceConfig,
    onDeleteBehaviorConfig,
    onProbeInferenceEndpoint,
    onCodexLogin,
    onCancelCodexLogin,
    onGrokLogin,
    onCancelGrokLogin,
    onSaveInferenceProfileConfig,
    onSaveSkillConfig,
    onSaveToolsConfig,
    onSaveToolServiceConfig,
    onTestToolService,
  };
}
