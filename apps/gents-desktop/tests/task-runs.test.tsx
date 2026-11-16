import { cleanup, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { TaskRunSummaryView } from "@source-inc/gents-desktop-client";
import { renderIn, testApp } from "./app-fixture";
import { TaskEditor } from "../src/ui/screens/agent/TasksPanel";
import { deployment } from "./config-panel-wiring/fixtures";

vi.mock("@/lib/router", () => ({
  href: (route: { name: string; nodeDid?: string; sessionId?: string }) =>
    route.name === "session" ? `#session/${route.nodeDid}/${route.sessionId}` : "#",
  navigate: vi.fn(),
}));
afterEach(cleanup);

const run = (
  requestId: string,
  createdAt: string,
  fields: Partial<TaskRunSummaryView> = {},
): TaskRunSummaryView => ({
  requestId,
  requestDocId: null,
  nodeDid: deployment.nodeDid,
  requesterDid: null,
  sessionId: `s-${requestId}`,
  agentDid: "default",
  lifecycleState: "completed",
  executionOrigin: null,
  causedByTriggerId: null,
  causedByTriggerKind: null,
  createdAt,
  ...fields,
});

describe("a task's runs", () => {
  it("lists each run newest first, by what started it, with a way to its session", () => {
    const task = {
      ...deployment.tasks[0],
      runHistory: [
        run("r1", "2026-11-04T09:00:00Z"),
        run("r2", "2026-11-04T11:00:00Z", {
          causedByTriggerId: "trigger-a",
          causedByTriggerKind: "schedule",
          lifecycleState: "waitingForBackend",
        }),
        run("r3", "2026-11-04T10:00:00Z", { sessionId: null }),
      ],
    };
    renderIn(testApp({}), <TaskEditor deployment={deployment} task={task} />);
    const runs = screen.getAllByText(/^By (hand|its schedule) · /);
    expect(runs.map((el) => el.textContent)).toEqual([
      "By its schedule · Waiting",
      "By hand · Done",
      "By hand · Done",
    ]);
    expect(screen.queryByText("r1")).toBeNull();
    const links = screen.getAllByRole("link", { name: "Open session" });
    expect(links.map((a) => a.getAttribute("href"))).toEqual([
      `#session/${deployment.nodeDid}/s-r2`,
      `#session/${deployment.nodeDid}/s-r1`,
    ]);
  });
});
