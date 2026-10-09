import { screen, waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import {
  createAgent,
  createBackend,
  createConfigFlowIds,
  createEventSource,
  createInferenceProfile,
  createSchedule,
  createTask,
  createTools,
  createToolService,
  createTriggerDocument,
  waitForConfigFlowReady,
} from "./tauri-driver-live/config-flow";
import {
  DEFAULT_LIVE_INFERENCE_URL,
  DEFAULT_LIVE_MODEL_NAME,
  withLiveDesktop,
} from "./tauri-driver-live/harness";
import {
  describeLive,
  expectCompletedSession,
  logTurn,
} from "./tauri-driver-live/helpers";

describeLive("Tauri app live bridge runner config flow", () => {
  it("configures backend profile tools agent task and runs it", async () => {
    await withLiveDesktop(async ({ runner, driver }) => {
      const ids = createConfigFlowIds();
      const inferenceUrl =
        process.env.GENTS_TAURI_LIVE_INFERENCE_URL ?? DEFAULT_LIVE_INFERENCE_URL;
      const modelName =
        process.env.GENTS_TAURI_LIVE_MODEL_NAME ?? DEFAULT_LIVE_MODEL_NAME;
      const fileToolRoot = `${runner.toolRoot}/workspace`;

      await driver.ready();
      await driver.openConfig();

      await createBackend({ runner, driver, ids, inferenceUrl, modelName });
      await createInferenceProfile({ runner, driver, ids });
      await createToolService({ runner, driver, ids });
      await createTools({ runner, driver, ids, fileToolRoot });
      await createAgent({ runner, driver, ids });
      await createTask({ runner, driver, ids });
      await createSchedule({ runner, driver, ids });
      await createEventSource({ runner, driver, ids });
      await createTriggerDocument({ runner, driver, ids });
      await waitForConfigFlowReady(runner, ids);

      await driver.openConfigSection("tasks");
      await driver.openConfigItem(ids.taskId);
      await driver.user.click(screen.getByRole("button", { name: "Run task" }));
      await waitFor(() => {
        expect(runner.taskRunResults).toHaveLength(1);
      });
      const taskRun = runner.taskRunResults[0];
      expect(taskRun.agentId).toBe(ids.agentId);
      logTurn(`task run submitted taskId=${ids.taskId} requestId=${taskRun.requestId}`);
      const session = await runner.waitForRequestCompletion(taskRun);
      if (session.turnState !== "completed") {
        const diagnostics = await runner.fetchRequestDiagnostics(
          taskRun.sessionId,
          taskRun.requestId,
        );
        throw new Error(
          `config task run failed diagnostics=${JSON.stringify(diagnostics)}`,
        );
      }
      expectCompletedSession("config task run", session);
      expect(session.latestRequestId).toBe(taskRun.requestId);
      const diagnostics = await runner.fetchRequestDiagnostics(
        taskRun.sessionId,
        taskRun.requestId,
      );
      expect(diagnostics.remote.inferenceDiagnosticsError).toBeNull();
      expect(diagnostics.remote.inferenceCalls).toEqual(
        expect.arrayContaining([
          expect.objectContaining({
            requestId: taskRun.requestId,
            requestDocId: taskRun.requestDocId,
            nodeDid: runner.nodeDid,
            backendId: ids.backendId,
            agentId: ids.agentId,
            callKind: "inference",
            callState: "completed",
          }),
        ]),
      );
    });
  }, 600_000);
});
