import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
  ToolServiceTestRequest,
  ToolServiceTestResult,
} from "@source-inc/gents-desktop-client";

import { actionFailure, shownFailure } from "./actionFailure";

/** Each configuration change the bridge takes, as a failure names it. */
const CHANGES = {
  saveNodeConfig: "save the node",
  setDefaultAgent: "set the default agent",
  saveAgentConfig: "save the agent",
  deleteAgentConfig: "delete the agent",
  saveSkillConfig: "save the skill",
  deleteSkillConfig: "delete the skill",
  deleteContextConfig: "delete the context",
  saveTaskConfig: "save the task",
  deleteTaskConfig: "delete the task",
  saveScheduleConfig: "save the schedule",
  deleteScheduleConfig: "delete the schedule",
  saveTriggerConfig: "save the trigger",
  deleteTriggerConfig: "delete the trigger",
  saveEventSourceConfig: "save the event source",
  deleteEventSourceConfig: "delete the event source",
  saveBackendConfig: "save the backend",
  deleteBackendConfig: "delete the backend",
  saveInferenceProfileConfig: "save the inference profile",
  deleteInferenceProfileConfig: "delete the inference profile",
  saveToolsConfig: "save the tools",
  deleteToolsConfig: "delete the tools",
  saveToolServiceConfig: "save the tool service",
  deleteToolServiceConfig: "delete the tool service",
  patchConfigComponents: "save the configuration",
  applyConfigComponents: "apply the configuration",
} as const satisfies Partial<Record<keyof DesktopApiAdapter, string>>;

export type ConfigChange = keyof typeof CHANGES;
export type ConfigRequest<K extends ConfigChange> = Parameters<DesktopApiAdapter[K]>[0];

type ConfigActionParams = {
  api: DesktopApiAdapter;
  /** shows a failed action to the person, once */
  reportFailure: (message: string) => void;
  mutateSnapshot: <T>(operation: () => Promise<T>) => Promise<T>;
};

export function createConfigActions({
  api,
  reportFailure,
  mutateSnapshot,
}: ConfigActionParams) {
  async function changeConfig<K extends ConfigChange>(
    change: K,
    request: ConfigRequest<K>,
    label: string = CHANGES[change],
  ): Promise<DesktopClientSnapshot> {
    const write = api[change] as (
      request: ConfigRequest<K>,
    ) => Promise<DesktopClientSnapshot>;
    try {
      return await mutateSnapshot(() => write(request));
    } catch (error) {
      reportFailure(actionFailure(label, error));
      throw shownFailure(error);
    }
  }

  return {
    /**
     * A configuration change: the bridge's write, then a fresh read once it
     * lands, and one report naming what failed: the change's own name, or
     * what the person did when the screen knows it better (a switch turned
     * on writes a patch). The caller still sees the failure, to keep what
     * the person typed.
     */
    changeConfig,
    /** Tries a tool service's connection without saving it. The screen that
        asks shows the answer, and a failure, its own way. */
    testToolService: (
      request: ToolServiceTestRequest,
    ): Promise<ToolServiceTestResult> => api.testToolService(request),
  };
}
