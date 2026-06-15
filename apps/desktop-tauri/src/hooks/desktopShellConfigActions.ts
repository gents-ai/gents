import type { Dispatch, SetStateAction } from "react";

import {
  saveAgentConfig,
  saveBackendConfig,
  saveBehaviorConfig,
  saveInferenceProfileConfig,
  saveSkillConfig,
  saveToolSelectionConfig,
  saveToolServiceConfig,
  testToolService,
} from "../lib/desktop-api";
import type {
  AgentConfigSaveRequest,
  BackendSaveRequest,
  BehaviorSaveRequest,
  DesktopClientSnapshot,
  InferenceProfileSaveRequest,
  SkillSaveRequest,
  ToolSelectionSaveRequest,
  ToolServiceSaveRequest,
  ToolServiceTestRequest,
  ToolServiceTestResult,
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
    onSaveInferenceProfileConfig,
    onSaveSkillConfig,
    onSaveToolSelectionConfig,
    onSaveToolServiceConfig,
    onTestToolService,
  };
}
