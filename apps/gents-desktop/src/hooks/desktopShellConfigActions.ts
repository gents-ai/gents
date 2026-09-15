import type { Dispatch, SetStateAction } from "react";

import type {
  AgentConfigSaveRequest,
  BackendSaveRequest,
  ConfigComponentsPatchRequest,
  ConfigComponentsApplyRequest,
  BehaviorSaveRequest,
  CodexLoginResult,
  DesktopApiAdapter,
  DesktopClientSnapshot,
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
import type { SnapshotPublication } from "./desktopSnapshotPublication";

type ConfigActionParams = {
  api: DesktopApiAdapter;
  setError: Dispatch<SetStateAction<string | null>>;
  setSavingBehaviorConfig: Dispatch<SetStateAction<boolean>>;
  setSavingConfig: Dispatch<SetStateAction<boolean>>;
  setSelectedAgentDid: Dispatch<SetStateAction<string | null>>;
  setSelectedBehaviorId: Dispatch<SetStateAction<string | null>>;
  beginSnapshotPublication: () => SnapshotPublication;
};

export function createDesktopShellConfigActions({
  api,
  setError,
  setSavingBehaviorConfig,
  setSavingConfig,
  setSelectedAgentDid,
  setSelectedBehaviorId,
  beginSnapshotPublication,
}: ConfigActionParams) {
  async function publishSnapshotResult(
    operation: () => Promise<DesktopClientSnapshot>,
  ) {
    const publication = beginSnapshotPublication();
    const next = await operation();
    publication.publish(next);
    return next;
  }
  async function onSaveAgentConfig(request: AgentConfigSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await publishSnapshotResult(() => api.saveAgentConfig(request));
      setSelectedAgentDid(request.document.agent_did);
      setSelectedBehaviorId(request.document.default_behavior_id ?? null);
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
      const next = await publishSnapshotResult(() => api.saveBehaviorConfig(request));
      setSelectedAgentDid(request.document.agent_did);
      setSelectedBehaviorId(request.document.behavior_id);
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
      const next = await publishSnapshotResult(() => api.saveSkillConfig(request));
      setSelectedAgentDid(request.document.agent_did);
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
      const next = await publishSnapshotResult(() => api.deleteSkillConfig(request));
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
      const next = await publishSnapshotResult(() => api.deleteTaskConfig(request));
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
      const next = await publishSnapshotResult(() => api.deleteScheduleConfig(request));
      setSelectedAgentDid(request.agentDid);
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
      const next = await publishSnapshotResult(() =>
        api.deleteEventSourceConfig(request),
      );
      setSelectedAgentDid(request.agentDid);
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
      const next = await publishSnapshotResult(() => api.deleteTriggerConfig(request));
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
      const next = await publishSnapshotResult(() => api.deleteBackendConfig(request));
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
      const next = await publishSnapshotResult(() =>
        api.deleteInferenceProfileConfig(request),
      );
      setSelectedAgentDid(request.agentDid);
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
      const next = await publishSnapshotResult(() => api.deleteToolsConfig(request));
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
      const next = await publishSnapshotResult(() =>
        api.deleteToolServiceConfig(request),
      );
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
      const next = await publishSnapshotResult(() => api.deleteBehaviorConfig(request));
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
      const next = await publishSnapshotResult(() => api.saveBackendConfig(request));
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
      const next = await publishSnapshotResult(() =>
        api.patchConfigComponents(request),
      );
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
      const next = await publishSnapshotResult(() =>
        api.applyConfigComponents(request),
      );
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
      const next = await publishSnapshotResult(() =>
        api.saveInferenceProfileConfig(request),
      );
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
      const next = await publishSnapshotResult(() => api.saveToolsConfig(request));
      setSelectedAgentDid(request.document.agent_did);
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
      const next = await publishSnapshotResult(() =>
        api.saveToolServiceConfig(request),
      );
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
