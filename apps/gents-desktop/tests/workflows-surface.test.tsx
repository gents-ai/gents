import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopSessionSnapshot,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import type { JobProgress } from "@source-inc/gents-desktop-client/generated/JobProgress";

import { JobProgressBody, JobStages } from "../src/ui/screens/jobParts";
import { WorkflowStrip, WorkflowsSurface } from "../src/ui/screens/WorkflowsSurface";
import { node, renderIn, testApp } from "./app-fixture";
import { harnessWorkflowRuns } from "./ui-harness/desktopHarness";

const NODE = "did:key:z6MkBombadilAgent";
const [running] = harnessWorkflowRuns();

const stages = (n: number, at: number): JobProgress["phases"] =>
  Array.from({ length: n }, (_, i) => ({
    title: `stage-${i + 1}`,
    state: i < at ? "done" : i === at ? "active" : "pending",
  }));

const progress = (over: Partial<JobProgress> = {}): JobProgress => ({
  ...running.progress!,
  ...over,
});

const row = (over: Partial<RenderedToolCallView> = {}): RenderedToolCallView =>
  ({
    itemKey: "workflow-review",
    toolName: "workflow",
    statusKind: "running",
    requestId: "request-intro",
    toolCallId: "call-workflow-review",
    toolCallDocId: running.row_doc_id,
    nodeDid: NODE,
    startedAt: "2026-06-16T20:00:00Z",
    presentation: { kind: "generic", input: null, output: null },
    reconstruction: { state: "ready" },
    jobProgress: running.progress,
    jobHandle: "review",
    jobState: "running",
    jobOrigin: "installed",
    ...over,
  }) as unknown as RenderedToolCallView;

function appWith(tools: RenderedToolCallView[]) {
  return testApp({
    api: { getJobView: vi.fn().mockResolvedValue(running) },
    deployments: [node({ nodeDid: NODE, source: "local" })],
    selection: { nodeDid: NODE, sessionId: "session-intro" },
    session: {
      sessionId: "session-intro",
      latestRequestId: "request-intro",
      timelineItems: tools.length
        ? [{ kind: "toolGroup", itemKey: "group", messageSequence: 1, tools }]
        : [],
    } as unknown as DesktopSessionSnapshot,
  });
}

describe("the session's Workflows tab", () => {
  it("says so when the session has started no workflow", () => {
    renderIn(
      appWith([]),
      <WorkflowsSurface placement="dock" sessionId="session-intro" />,
    );
    expect(screen.getByText("No workflows in this session")).toBeVisible();
  });

  it("opens a session's only run straight away, with its stages", async () => {
    renderIn(
      appWith([row()]),
      <WorkflowsSurface placement="dock" sessionId="session-intro" />,
    );
    expect(await screen.findByTestId("run-view")).toBeTruthy();
    const stagesList = await screen.findByTestId("job-phases");
    expect(stagesList.textContent).toContain("review");
    expect(screen.queryByRole("button", { name: "Back" })).toBeNull();
  });

  it("lists several runs, those still going first, and opens one with a way back", async () => {
    const done = row({
      itemKey: "workflow-done",
      toolCallDocId: "doc-done",
      jobState: "completed",
      statusKind: "success",
      jobProgress: progress({ title: "Earlier review" }),
      startedAt: "2026-06-17T20:00:00Z",
    } as never);
    renderIn(
      appWith([done, row()]),
      <WorkflowsSurface placement="dock" sessionId="session-intro" />,
    );
    const rows = screen.getAllByTestId("workflow-run-row");
    expect(rows.map((r) => r.textContent)).toEqual([
      expect.stringContaining("Review the diff"),
      expect.stringContaining("Earlier review"),
    ]);
    fireEvent.click(rows[0]!);
    expect(await screen.findByTestId("run-view")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(screen.getAllByTestId("workflow-run-row")).toHaveLength(2);
  });
});

describe("the workflow strip above the composer", () => {
  it("shows a running workflow's stage, and nothing once none is going", () => {
    const app = appWith([row()]);
    const view = renderIn(app, <WorkflowStrip sessionId="session-intro" />);
    expect(screen.getByTestId("workflow-strip").textContent).toContain(
      "review · stage 2 of 3",
    );
    view.unmount();
    renderIn(
      appWith([row({ jobState: "completed", statusKind: "success" } as never)]),
      <WorkflowStrip sessionId="session-intro" />,
    );
    expect(screen.queryByTestId("workflow-strip")).toBeNull();
  });

  it("says when a workflow waits on the person", () => {
    renderIn(
      appWith([row({ jobState: "awaiting_user" })]),
      <WorkflowStrip sessionId="session-intro" />,
    );
    expect(screen.getByTestId("workflow-strip").textContent).toContain(
      "Waiting on your answer",
    );
  });

  it("counts several running workflows", () => {
    renderIn(
      appWith([row(), row({ itemKey: "two", toolCallDocId: "doc-two" })]),
      <WorkflowStrip sessionId="session-intro" />,
    );
    expect(screen.getByTestId("workflow-strip").textContent).toContain(
      "2 workflows running",
    );
  });
});

describe("many stages", () => {
  it("names every stage while they fit, and only the current one past four", () => {
    const app = testApp({});
    const few = renderIn(
      app,
      <JobProgressBody state="running" progress={progress({ phases: stages(3, 1) })} />,
    );
    expect(screen.queryByTestId("job-stage")).toBeNull();
    few.unmount();
    renderIn(
      app,
      <JobProgressBody
        state="running"
        progress={progress({ phases: stages(12, 6) })}
      />,
    );
    expect(screen.getByTestId("job-stage").textContent).toContain(
      "stage-7· stage 7 of 12",
    );
    expect(screen.getAllByRole("listitem")).toHaveLength(12);
  });

  it("folds the done stages before the last two into one row that opens", () => {
    render(<JobStages progress={progress({ phases: stages(9, 6), active: [] })} />);
    const folded = screen.getByTestId("job-stages-folded");
    expect(folded.textContent).toBe("4 stages done");
    expect(screen.queryByText("stage-1")).toBeNull();
    expect(screen.getByText("stage-5")).toBeTruthy();
    act(() => folded.click());
    expect(screen.getByText("stage-1")).toBeTruthy();
    expect(screen.queryByTestId("job-stages-folded")).toBeNull();
  });

  it("keeps a short run's stages all in view", () => {
    render(<JobStages progress={progress({ phases: stages(3, 2), active: [] })} />);
    expect(screen.queryByTestId("job-stages-folded")).toBeNull();
    expect(within(screen.getByTestId("job-phases")).getByText("stage-1")).toBeTruthy();
  });
});
