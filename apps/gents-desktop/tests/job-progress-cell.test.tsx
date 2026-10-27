import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import type { JobProgress } from "@source-inc/gents-desktop-client/generated/JobProgress";

import { JobProgressCell } from "../src/ui/screens/JobProgressCell";
import { node, renderIn, testApp } from "./app-fixture";

const progress = (over: Partial<JobProgress> = {}): JobProgress => ({
  title: "review the diff",
  state: "running",
  phases: [
    { title: "plan", state: "done" },
    { title: "execute", state: "active" },
    { title: "verify", state: "pending" },
  ],
  active: [],
  spawned: 2,
  budget: { total: 5, spent: 2, remaining: 3 },
  last_event: "agent 2 started",
  observed_terminal_children: 0,
  revision: 3,
  ...over,
});

const row = (over: Partial<RenderedToolCallView> = {}): RenderedToolCallView =>
  ({
    itemKey: "tool-1",
    toolName: "workflow",
    status: "running",
    statusKind: "running",
    toolCallId: "call-1",
    nodeDid: "did:test:row-owner",
    toolCallDocId: "physical-job-in-another-session",
    presentation: { kind: "generic", input: null, output: null },
    reconstruction: { state: "ready" },
    jobProgress: progress(),
    jobHandle: "wf-1",
    jobState: "running",
    ...over,
  }) as unknown as RenderedToolCallView;

function mount(
  tool: RenderedToolCallView,
  api: object = {},
  owner = { nodeDid: "did:test:row-owner", source: "local" },
) {
  const app = testApp({
    api,
    deployments: [node(owner)],
    selection: { nodeDid: owner.nodeDid, sessionId: "session-1" },
  });
  renderIn(
    app,
    <>
      <div data-testid="composer">
        <textarea aria-label="composer" />
      </div>
      <JobProgressCell tool={tool} />
    </>,
  );
  return app;
}

const text = (id: string) => screen.getByTestId(id).textContent;

describe("JobProgressCell", () => {
  it("shows a running row with its phases, agent bar, last event, Stop and Pause", () => {
    mount(row());
    expect(text("job-state")).toBe("running");
    expect(text("job-phases")).toContain("plan");
    expect(text("job-phases")).toContain("verify");
    expect(text("job-agents")).toBe("2 of 5 agents");
    expect(text("job-last-event")).toBe("agent 2 started");
    expect(screen.getByRole("button", { name: "Stop" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Pause" })).toBeTruthy();
  });

  it("takes the state from jobState, not from the progress record", () => {
    mount(row({ jobState: "cancelled" }));
    expect(text("job-state")).toBe("cancelled");
    expect(screen.queryByRole("button", { name: "Stop" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Pause" })).toBeNull();
  });

  it("shows why a paused row paused, with the budget unchanged", () => {
    mount(
      row({
        jobState: "paused",
        jobProgress: progress({
          state: "paused",
          pause_reason: "budget_exceeded",
          budget: { total: 5, spent: 5, remaining: 0 },
        }),
      }),
    );
    expect(text("job-state")).toBe("paused");
    expect(text("job-pause-reason")).toBe("budget exceeded");
    expect(text("job-agents")).toBe("5 of 5 agents");
  });

  it("prefills the composer from Answer on an awaiting row", () => {
    const app = mount(
      row({
        jobState: "awaiting_user",
        jobProgress: progress({ state: "awaiting_user", question: "Which branch?" }),
      }),
    );
    expect(text("job-question")).toBe("Which branch?");
    fireEvent.click(screen.getByRole("button", { name: "Answer" }));
    const key = app.view.getState().draftKey;
    expect(app.drafts.getState().drafts[key]).toBe("Answer for wf-1: ");
    expect(document.activeElement).toBe(screen.getByLabelText("composer"));
  });

  it("shows the outcome code and the given next calls on a terminal row", () => {
    mount(
      row({
        statusKind: "error",
        jobState: "failed",
        jobOutcome: { code: "plugin_job_budget", message: "out of budget" },
        jobNextCalls: ["workflow result wf-1", "workflow resume wf-1"],
      }),
    );
    expect(text("job-outcome")).toBe("plugin_job_budget: out of budget");
    expect(screen.getByTestId("job-next-calls").children).toHaveLength(2);
    expect(screen.getByTestId("job-next-calls").children[0].textContent).toBe(
      "workflow result wf-1",
    );
    expect(screen.getByTestId("job-next-calls").children[1].textContent).toBe(
      "workflow resume wf-1",
    );
    expect(screen.queryByRole("button", { name: "Stop" })).toBeNull();
  });

  it("shows the state, and the next calls it is given, on a terminal row without an outcome", () => {
    mount(row({ jobState: "completed", jobNextCalls: ["workflow result wf-1"] }));
    expect(text("job-outcome")).toBe("completed");
    expect(text("job-next-calls")).toBe("workflow result wf-1");
  });

  it("prints whatever next calls it is given, verbatim and in order", () => {
    mount(row({ jobState: "completed", jobNextCalls: ["zeta  1", "alpha", "mid\tx"] }));
    const calls = Array.from(screen.getByTestId("job-next-calls").children).map(
      (c) => c.textContent,
    );
    expect(calls).toEqual(["zeta  1", "alpha", "mid\tx"]);
  });

  it.each([
    ["Stop", "stop"],
    ["Pause", "pause"],
  ])("%s calls desktop_job_control", async (label, action) => {
    const jobControl = vi
      .fn()
      .mockResolvedValue({ AlreadyTerminal: { state: "cancelled" } });
    mount(row(), { jobControl });
    fireEvent.click(screen.getByRole("button", { name: label }));
    await waitFor(() =>
      expect(jobControl).toHaveBeenCalledWith({
        nodeDid: "did:test:row-owner",
        toolCallDocId: "physical-job-in-another-session",
        action,
      }),
    );
  });
  it("disables control until the displayed row has physical identity", () => {
    const jobControl = vi.fn();
    mount(row({ toolCallDocId: null }), { jobControl });
    expect(screen.getByRole("button", { name: "Stop" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Pause" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(jobControl).not.toHaveBeenCalled();
  });

  it("offers Promote only on a completed dynamic run", () => {
    const { unmount } = renderIn(
      testApp({ selection: { sessionId: "session-1" } }),
      <JobProgressCell tool={row({ jobOrigin: "dynamic" })} />,
    );
    expect(screen.queryByRole("button", { name: "Promote" })).toBeNull();
    unmount();
    for (const jobState of ["failed", "cancelled", "timed_out"] as const) {
      const notCompleted = renderIn(
        testApp({ selection: { sessionId: "session-1" } }),
        <JobProgressCell tool={row({ jobState, jobOrigin: "dynamic" })} />,
      );
      expect(screen.queryByRole("button", { name: "Promote" })).toBeNull();
      notCompleted.unmount();
    }
    const ended = renderIn(
      testApp({ selection: { sessionId: "session-1" } }),
      <JobProgressCell tool={row({ jobState: "completed", jobOrigin: "installed" })} />,
    );
    expect(screen.queryByRole("button", { name: "Promote" })).toBeNull();
    ended.unmount();
    const getDynamicWorkflow = vi.fn().mockReturnValue(new Promise(() => {}));
    mount(row({ jobState: "completed", jobOrigin: "dynamic" }), { getDynamicWorkflow });
    fireEvent.click(screen.getByRole("button", { name: "Promote" }));
    expect(getDynamicWorkflow).toHaveBeenCalledWith("session-1:call-1");
  });

  it("drops Promote once the run's script has been promoted", async () => {
    const getDynamicWorkflow = vi
      .fn()
      .mockResolvedValue({ promoted_as: ["gents/rome"] });
    mount(row({ jobState: "completed", jobOrigin: "dynamic" }), { getDynamicWorkflow });
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "Promote" })).toBeNull(),
    );
    expect(getDynamicWorkflow).toHaveBeenCalledWith("session-1:call-1");
  });

  it("only looks up/promotes a run when the selected node owns the row and is local", async () => {
    const getDynamicWorkflow = vi.fn().mockResolvedValue({ promoted_as: [] });
    const local = renderIn(
      testApp({
        api: { getDynamicWorkflow },
        deployments: [node({ nodeDid: "did:test:row-owner", source: "local" })],
        selection: { nodeDid: "did:test:row-owner", sessionId: "session-1" },
      }),
      <JobProgressCell tool={row({ jobState: "completed", jobOrigin: "dynamic" })} />,
    );
    expect(await screen.findByRole("button", { name: "Promote" })).toBeTruthy();
    expect(getDynamicWorkflow).toHaveBeenCalledWith("session-1:call-1");
    local.unmount();

    const remoteLookup = vi.fn();
    const remote = renderIn(
      testApp({
        api: { getDynamicWorkflow: remoteLookup },
        deployments: [node({ nodeDid: "did:test:remote", source: "enrollment" })],
        selection: { nodeDid: "did:test:remote", sessionId: "session-1" },
      }),
      <JobProgressCell
        tool={row({
          nodeDid: "did:test:remote",
          jobState: "completed",
          jobOrigin: "dynamic",
        })}
      />,
    );
    expect(screen.queryByRole("button", { name: "Promote" })).toBeNull();
    expect(remoteLookup).not.toHaveBeenCalled();
    remote.unmount();
  });

  it("closes an open local promotion dialog when the selected owner changes", async () => {
    const getDynamicWorkflow = vi.fn().mockReturnValue(new Promise(() => {}));
    const localNode = node({ nodeDid: "did:test:row-owner", source: "local" });
    const remoteNode = node({ nodeDid: "did:test:remote", source: "enrollment" });
    const app = testApp({
      api: { getDynamicWorkflow },
      deployments: [localNode, remoteNode],
      selection: { nodeDid: localNode.nodeDid, sessionId: "session-1" },
    });
    const view = renderIn(
      app,
      <JobProgressCell tool={row({ jobState: "completed", jobOrigin: "dynamic" })} />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Promote" }));
    expect(await screen.findByRole("dialog")).toBeTruthy();
    expect(getDynamicWorkflow).toHaveBeenCalledTimes(2);

    await act(async () => {
      app.stores.selection.setState({ nodeDid: remoteNode.nodeDid });
      view.rerender(
        <JobProgressCell
          tool={row({
            nodeDid: remoteNode.nodeDid,
            jobState: "completed",
            jobOrigin: "dynamic",
          })}
        />,
      );
    });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByRole("button", { name: "Promote" })).toBeNull();
    expect(getDynamicWorkflow).toHaveBeenCalledTimes(2);
  });

  it("does not apply an older lookup after the row selector changes", async () => {
    let finishOld!: (value: { promoted_as: string[] }) => void;
    const getDynamicWorkflow = vi.fn((selector: string) =>
      selector === "session-1:call-old"
        ? new Promise<{ promoted_as: string[] }>((resolve) => {
            finishOld = resolve;
          })
        : Promise.resolve({ promoted_as: [] }),
    );
    const app = testApp({
      api: { getDynamicWorkflow },
      deployments: [node({ nodeDid: "did:test:row-owner", source: "local" })],
      selection: { nodeDid: "did:test:row-owner", sessionId: "session-1" },
    });
    const view = renderIn(
      app,
      <JobProgressCell
        tool={row({
          toolCallId: "call-old",
          jobState: "completed",
          jobOrigin: "dynamic",
        })}
      />,
    );
    await waitFor(() =>
      expect(getDynamicWorkflow).toHaveBeenCalledWith("session-1:call-old"),
    );
    view.rerender(
      <JobProgressCell
        tool={row({
          toolCallId: "call-new",
          jobState: "completed",
          jobOrigin: "dynamic",
        })}
      />,
    );
    await waitFor(() =>
      expect(getDynamicWorkflow).toHaveBeenCalledWith("session-1:call-new"),
    );
    expect(await screen.findByRole("button", { name: "Promote" })).toBeTruthy();
    await act(async () => finishOld({ promoted_as: ["gents/old"] }));
    expect(screen.getByRole("button", { name: "Promote" })).toBeTruthy();
  });
});
