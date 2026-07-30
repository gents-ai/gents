import type { Dispatch, SetStateAction } from "react";

import type {
  AgentConfigSaveRequest,
  BackendSaveRequest,
  BehaviorSaveRequest,
  CodexLoginResult,
  DesktopApiAdapter,
  DesktopClientSnapshot,
  InferenceProbeResult,
  InferenceProfileSaveRequest,
  SkillDeleteRequest,
  SkillSaveRequest,
  ToolSelectionSaveRequest,
  ToolServiceSaveRequest,
  ToolServiceTestRequest,
  ToolServiceTestResult,
  TaskDeleteRequest,
  ScheduleDeleteRequest,
  EventTriggerDeleteRequest,
  BackendDeleteRequest,
  InferenceProfileDeleteRequest,
  ToolSelectionDeleteRequest,
  ToolServiceDeleteRequest,
  BehaviorDeleteRequest,
} from "@source-inc/gents-desktop-client";

type ConfigActionParams = {
  api: DesktopApiAdapter;
  setError: Dispatch<SetStateAction<string | null>>;
  setSavingBehaviorConfig: Dispatch<SetStateAction<boolean>>;
  setSavingConfig: Dispatch<SetStateAction<boolean>>;
  setSelectedAgentDid: Dispatch<SetStateAction<string | null>>;
  setSelectedBehaviorId: Dispatch<SetStateAction<string | null>>;
  setSnapshot: Dispatch<SetStateAction<DesktopClientSnapshot | null>>;
};

export function createDesktopShellConfigActions({
  api,
  setError,
  setSavingBehaviorConfig,
  setSavingConfig,
  setSelectedAgentDid,
  setSelectedBehaviorId,
  setSnapshot,
}: ConfigActionParams) {
  async function onSaveAgentConfig(request: AgentConfigSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await api.saveAgentConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
      setSelectedBehaviorId(request.defaultBehaviorId);
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
      const next = await api.saveBehaviorConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
      setSelectedBehaviorId(request.behaviorId);
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
      const next = await api.saveSkillConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
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
      const next = await api.deleteSkillConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
      return next;
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
      const next = await api.deleteTaskConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
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
      const next = await api.deleteScheduleConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onDeleteEventTriggerConfig(request: EventTriggerDeleteRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await api.deleteEventTriggerConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
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
      const next = await api.deleteBackendConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
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
      const next = await api.deleteInferenceProfileConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onDeleteToolSelectionConfig(request: ToolSelectionDeleteRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await api.deleteToolSelectionConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
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
      const next = await api.deleteToolServiceConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
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
      const next = await api.deleteBehaviorConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
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
      const next = await api.saveBackendConfig(request);
      setSnapshot(next);
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

  async function onSaveInferenceProfileConfig(request: InferenceProfileSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await api.saveInferenceProfileConfig(request);
      setSnapshot(next);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onSaveToolSelectionConfig(request: ToolSelectionSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await api.saveToolSelectionConfig(request);
      setSnapshot(next);
      setSelectedAgentDid(request.agentDid);
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
      const next = await api.saveToolServiceConfig(request);
      setSnapshot(next);
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
    onSaveBehaviorConfig,
    onDeleteSkillConfig,
    onDeleteTaskConfig,
    onDeleteScheduleConfig,
    onDeleteEventTriggerConfig,
    onDeleteBackendConfig,
    onDeleteInferenceProfileConfig,
    onDeleteToolSelectionConfig,
    onDeleteToolServiceConfig,
    onDeleteBehaviorConfig,
    onProbeInferenceEndpoint,
    onCodexLogin,
    onCancelCodexLogin,
    onSaveInferenceProfileConfig,
    onSaveSkillConfig,
    onSaveToolSelectionConfig,
    onSaveToolServiceConfig,
    onTestToolService,
  };
}
