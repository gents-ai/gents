import type { Dispatch, SetStateAction } from "react";

import {
  deleteSkillConfig,
  saveAgentConfig,
  saveBackendConfig,
  saveBehaviorConfig,
  saveInferenceProfileConfig,
  saveSkillConfig,
  saveToolSelectionConfig,
  saveToolServiceConfig,
  testToolService,
  deleteTaskConfig,
  deleteScheduleConfig,
  deleteEventTriggerConfig,
  deleteBackendConfig,
  deleteInferenceProfileConfig,
  deleteToolSelectionConfig,
  deleteToolServiceConfig,
  deleteBehaviorConfig,
} from "../lib/desktop-api";
import type {
  AgentConfigSaveRequest,
  BackendSaveRequest,
  BehaviorSaveRequest,
  DesktopClientSnapshot,
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
} from "../lib/types";

type ConfigActionParams = {
  setError: Dispatch<SetStateAction<string | null>>;
  setSavingBehaviorConfig: Dispatch<SetStateAction<boolean>>;
  setSavingConfig: Dispatch<SetStateAction<boolean>>;
  setSelectedAgentDid: Dispatch<SetStateAction<string | null>>;
  setSelectedBehaviorId: Dispatch<SetStateAction<string | null>>;
  setSnapshot: Dispatch<SetStateAction<DesktopClientSnapshot | null>>;
};

export function createDesktopShellConfigActions({
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
      const next = await saveAgentConfig(request);
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
      const next = await saveBehaviorConfig(request);
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
      const next = await saveSkillConfig(request);
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
      const next = await deleteSkillConfig(request);
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
      const next = await deleteTaskConfig(request);
      setSnapshot(next);
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
      const next = await deleteScheduleConfig(request);
      setSnapshot(next);
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
      const next = await deleteEventTriggerConfig(request);
      setSnapshot(next);
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
      const next = await deleteBackendConfig(request);
      setSnapshot(next);
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
      const next = await deleteInferenceProfileConfig(request);
      setSnapshot(next);
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
      const next = await deleteToolSelectionConfig(request);
      setSnapshot(next);
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
      const next = await deleteToolServiceConfig(request);
      setSnapshot(next);
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
      const next = await deleteBehaviorConfig(request);
      setSnapshot(next);
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
      const next = await saveBackendConfig(request);
      setSnapshot(next);
      return next;
    } catch (err) {
      setError(String(err));
      throw err;
    } finally {
      setSavingConfig(false);
    }
  }

  async function onSaveInferenceProfileConfig(request: InferenceProfileSaveRequest) {
    setSavingConfig(true);
    setError(null);
    try {
      const next = await saveInferenceProfileConfig(request);
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
      const next = await saveToolSelectionConfig(request);
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
      const next = await saveToolServiceConfig(request);
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
      return await testToolService(request);
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
    onSaveInferenceProfileConfig,
    onSaveSkillConfig,
    onSaveToolSelectionConfig,
    onSaveToolServiceConfig,
    onTestToolService,
  };
}
