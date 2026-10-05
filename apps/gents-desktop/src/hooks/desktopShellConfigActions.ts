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
  setError: (error: string | null) => void;
  mutateSnapshot: <T>(operation: () => Promise<T>) => Promise<T>;
};

export function createDesktopShellConfigActions({
  api,
  setError,
  mutateSnapshot,
}: ConfigActionParams) {
  /**
   * A configuration change: the bridge's write, then a fresh read once it
   * lands, and one report naming what failed. The caller still sees the
   * failure, to keep what the person typed.
   */
  async function changeConfig<K extends ConfigChange>(
    change: K,
    request: ConfigRequest<K>,
  ): Promise<DesktopClientSnapshot> {
    const write = api[change] as (
      request: ConfigRequest<K>,
    ) => Promise<DesktopClientSnapshot>;
    setError(null);
    try {
      return await mutateSnapshot(() => write(request));
    } catch (error) {
      setError(actionFailure(CHANGES[change], error));
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
    changeConfig,
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
