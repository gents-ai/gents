import type {
  DesktopApiAdapter,
  ScheduleRunRequest,
  TaskRunRequest,
  TaskRunResult,
} from "@source-inc/gents-desktop-client";
import { actionFailure, shownFailure } from "./actionFailure";
import { selection, type SelectionStore } from "./selectionStore";

type TaskActionParams = {
  api: DesktopApiAdapter;
  store: SelectionStore;
  refreshSnapshot: () => Promise<void>;
  reportFailure: (message: string) => void;
};

export function createTaskActions({
  api,
  store,
  refreshSnapshot,
  reportFailure,
}: TaskActionParams) {
  async function run(label: string, start: () => Promise<TaskRunResult>) {
    const intentGeneration = selection.captureIntent(store);
    try {
      const result = await start();
      await refreshSnapshot();
      return result;
    } catch (err) {
      if (!selection.acceptsIntent(store, intentGeneration)) throw err;
      reportFailure(actionFailure(label, err));
      throw shownFailure(err);
    }
  }

  return {
    runSchedule: (request: ScheduleRunRequest) =>
      run("run the schedule", () => api.runSchedule(request)),
    runTask: (request: TaskRunRequest) =>
      run("run the task", () => api.runTask(request)),
  };
}
