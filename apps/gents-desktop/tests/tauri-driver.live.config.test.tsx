import { screen, waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import {
  createBackend,
  createBehavior,
  createConfigFlowIds,
  createEventTrigger,
  createInferenceProfile,
  createSchedule,
  createTask,
  createToolSelection,
  createToolService,
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
  it("configures backend profile tools behavior task and runs it", async () => {
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
      await createToolSelection({ runner, driver, ids, fileToolRoot });
      await createBehavior({ runner, driver, ids });
      await createTask({ runner, driver, ids });
      await createSchedule({ runner, driver, ids });
      await createEventTrigger({ driver, ids });
      await waitForConfigFlowReady(runner, ids);

      await driver.openConfigSection("tasks");
      await driver.user.click(screen.getByTestId("task-run"));
      await waitFor(() => {
        expect(runner.taskRunResults).toHaveLength(1);
      });
      const taskRun = runner.taskRunResults[0];
      expect(taskRun.behaviorId).toBe(ids.behaviorId);
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
    });
  }, 600_000);
});
