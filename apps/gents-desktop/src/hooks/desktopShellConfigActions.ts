import type {
  CodexLoginResult,
  DesktopApiAdapter,
  DesktopClientSnapshot,
  ToolServiceTestRequest,
  ToolServiceTestResult,
} from "@source-inc/gents-desktop-client";

import { actionFailure, shownFailure } from "./desktopShellRuntime";

/** Each configuration change the bridge takes, as a failure names it. */
const CHANGES = {
  saveAgentConfig: "save the agent",
  setDefaultBehavior: "set the default behavior",
  saveBehaviorConfig: "save the behavior",
  deleteBehaviorConfig: "delete the behavior",
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

export function createDesktopShellConfigActions({
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

  /** A call that stores nothing: reported the same way, without a re-read. */
  async function call<T>(label: string, run: () => Promise<T>) {
    try {
      return await run();
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
    /** Signs the agent in to Codex in the browser; nothing is stored here.
        A failure is reported once, then rethrown. */
    codexLogin: (agentDid: string): Promise<CodexLoginResult> =>
      call("sign in to Codex", () => api.codexLogin(agentDid)),
    /** Abandons a Codex sign-in whose browser was closed. Best effort: a
        failure (nothing in flight, say) never blocks closing the wizard. */
    cancelCodexLogin: (): Promise<void> => api.cancelCodexLogin().catch(() => {}),
    /** Signs the agent in to Grok in the browser; nothing is stored here.
        A failure is reported once, then rethrown. */
    grokLogin: (agentDid: string) =>
      call("sign in to Grok", () => api.grokLogin(agentDid)),
    /** Abandons a Grok sign-in whose browser was closed. Best effort, as
        for Codex. */
    cancelGrokLogin: (): Promise<void> => api.cancelGrokLogin().catch(() => {}),
    /** Tries a tool service's connection without saving it. A failure is
        reported once, then rethrown. */
    testToolService: (
      request: ToolServiceTestRequest,
    ): Promise<ToolServiceTestResult> =>
      call("test the tool service", () => api.testToolService(request)),
  };
}
