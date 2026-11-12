import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import type { JobView } from "@source-inc/gents-desktop-client/generated/JobView";

import { sessionDraftKey } from "../src/hooks/shellView";
import { href } from "../src/ui/lib/router";
import { JobProgressCell } from "../src/ui/screens/JobProgressCell";
import { node, renderIn, testApp } from "./app-fixture";
import {
  createDesktopUiHarness,
  harnessWorkflowRuns,
} from "./ui-harness/desktopHarness";

const NODE = "did:key:z6MkBombadilAgent";
const [running, prior] = harnessWorkflowRuns();

const run = (over: Partial<JobView> = {}): JobView => ({ ...running, ...over });

const row = (over: Partial<RenderedToolCallView> = {}): RenderedToolCallView =>
  ({
    itemKey: "workflow-review",
    toolName: "workflow",
    statusKind: "running",
    toolCallId: "call-workflow-review",
    toolCallDocId: running.row_doc_id,
    nodeDid: NODE,
    presentation: { kind: "generic", input: null, output: null },
    reconstruction: { state: "ready" },
    jobProgress: running.progress,
    jobHandle: "review",
    jobState: "running",
    jobOrigin: "installed",
    ...over,
  }) as unknown as RenderedToolCallView;

function mount(
  getJobView: (request: { rowDocId?: string }) => Promise<JobView>,
  api: object = {},
  tool: RenderedToolCallView = row(),
  owner: { nodeDid: string; source: string } = { nodeDid: NODE, source: "local" },
) {
  const app = testApp({
    api: { getJobView, ...api },
    deployments: [node(owner)],
    /* no session open: in one, Open run opens the run in the session's
       panel instead (workflows-surface.test.tsx) */
    selection: { nodeDid: owner.nodeDid, sessionId: null },
  });
  const view = renderIn(app, cell(tool));
  return { app, view };
}

function cell(tool: RenderedToolCallView) {
  return (
    <>
      <div data-testid="composer">
        <textarea aria-label="composer" />
      </div>
      <JobProgressCell tool={tool} />
    </>
  );
}

async function openRun() {
  fireEvent.click(screen.getByRole("button", { name: "Open run" }));
  const sheet = await screen.findByRole("dialog");
  await within(sheet).findByTestId("run-what");
  return sheet;
}

describe("WorkflowRunSheet", () => {
  it("opens from the cell and reads the row by its physical id", async () => {
    const getJobView = vi.fn().mockResolvedValue(run());
    mount(getJobView);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(getJobView).not.toHaveBeenCalled();
    const sheet = await openRun();
    expect(getJobView).toHaveBeenCalledWith({
      nodeDid: NODE,
      rowDocId: running.row_doc_id,
    });
    expect(
      within(sheet).getByRole("heading", { name: "Review the diff" }),
    ).toBeTruthy();
  });

  it("serves the harness run and its resume chain deterministically", async () => {
    const { adapter } = createDesktopUiHarness({ scenario: "workflows" });
    await expect(
      adapter.getJobView({ nodeDid: NODE, rowDocId: running.row_doc_id }),
    ).resolves.toEqual(running);
    await expect(
      adapter.getJobView({ nodeDid: NODE, address: prior.address }),
    ).resolves.toEqual(prior);
    await expect(
      adapter.listJobs({
        nodeDid: NODE,
        states: ["running", "paused", "awaiting_user"],
      }),
    ).resolves.toEqual([running]);
    await expect(
      adapter.getJobView({ nodeDid: NODE, rowDocId: "missing" }),
    ).rejects.toThrow("not visible");
  });

  it("shows a running run's header, progress, live children first and Pause and Stop", async () => {
    mount(vi.fn().mockResolvedValue(run()));
    const sheet = await openRun();
    expect(within(sheet).getByTestId("job-state").textContent).toBe("running");
    expect(within(sheet).getByTestId("run-what").textContent).toBe("gents/review");
    const times = within(sheet).getByTestId("run-times").textContent;
    expect(times).toContain("Started");
    expect(times).not.toContain("Finished");
    expect(within(sheet).getByTestId("job-phases").textContent).toContain("review");
    expect(within(sheet).getByTestId("job-agents").textContent).toBe("2 of 4 agents");
    expect(within(sheet).getByTestId("job-last-event").textContent).toBe(
      "agent 2 started",
    );
    const children = within(sheet).getAllByTestId("run-child");
    expect(children.map((c) => c.dataset.state)).toEqual(["live", "completed"]);
    expect(children[0]!.textContent).toContain("Review the diff");
    expect(children[0]!.textContent).toContain("31.2k tokens");
    expect(children[0]!.textContent).toContain("3m 32s");
    expect(children[1]!.textContent).toContain("Run the checks");
    expect(children[1]!.textContent).toContain("12.4k tokens");
    expect(children[1]!.textContent).toContain("1m 35s");
    for (const name of ["Pause", "Stop"])
      expect(within(sheet).getByRole("button", { name })).toBeTruthy();
    for (const name of ["Promote", "Retry", "Resume", "Answer"])
      expect(within(sheet).queryByRole("button", { name })).toBeNull();
    expect(within(sheet).queryByTestId("run-outcome")).toBeNull();
  });

  it("links each child to its session", async () => {
    mount(vi.fn().mockResolvedValue(run()));
    const sheet = await openRun();
    const links = within(within(sheet).getByTestId("run-children")).getAllByRole(
      "link",
    );
    expect(links.map((link) => link.getAttribute("href"))).toEqual([
      href({ name: "session", sessionId: "session-workflow-review" }),
      href({ name: "session", sessionId: "session-workflow-verify" }),
    ]);
  });

  it.each([
    ["Pause", "pause"],
    ["Stop", "stop"],
  ])("%s controls the run's row", async (label, action) => {
    const jobControl = vi.fn().mockResolvedValue({ Applied: {} });
    mount(vi.fn().mockResolvedValue(run()), { jobControl });
    const sheet = await openRun();
    fireEvent.click(within(sheet).getByRole("button", { name: label }));
    await waitFor(() =>
      expect(jobControl).toHaveBeenCalledWith({
        nodeDid: NODE,
        toolCallDocId: running.row_doc_id,
        action,
      }),
    );
  });

  it.each(["failed", "timed_out"] as const)(
    "shows a %s run's outcome code and message, with no controls",
    async (state) => {
      mount(
        vi.fn().mockResolvedValue(
          run({
            state,
            outcome: {
              code: "plugin_job_budget",
              message: "The run spent its agent budget.",
            },
            completed_at: running.created_at + 125_000,
          }),
        ),
      );
      const sheet = await openRun();
      expect(within(sheet).getByTestId("job-state").textContent).toBe(
        state.replace("_", " "),
      );
      expect(within(sheet).getByTestId("run-outcome").textContent).toBe(
        "plugin_job_budget The run spent its agent budget.",
      );
      expect(within(sheet).getByTestId("run-times").textContent).toContain("Finished");
      expect(within(sheet).getByTestId("run-duration").textContent).toBe("2m 5s");
      for (const name of ["Pause", "Stop", "Promote", "Retry", "Resume"])
        expect(within(sheet).queryByRole("button", { name })).toBeNull();
    },
  );

  it("asks the question of a run awaiting the person; Answer starts the draft and closes", async () => {
    const waiting = run({
      state: "awaiting_user",
      progress: { ...running.progress!, question: "Which branch?" },
    });
    const { app } = mount(vi.fn().mockResolvedValue(waiting));
    const sheet = await openRun();
    expect(within(sheet).getByTestId("run-question").textContent).toBe("Which branch?");
    expect(within(sheet).queryByRole("button", { name: "Stop" })).toBeNull();
    fireEvent.click(within(sheet).getByRole("button", { name: "Answer" }));
    /* the run's own session's draft, opened from outside it */
    const key = sessionDraftKey(waiting.scope.node_did, waiting.session_id);
    expect(app.drafts.getState().drafts[key]).toBe("Answer for review: ");
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("offers Promote only on a completed one-off script on a local node", async () => {
    const script = run({
      state: "completed",
      completed_at: running.created_at + 1_000,
      pin: { ...running.pin, origin: "dynamic" },
    });
    const getDynamicWorkflow = vi.fn().mockReturnValue(new Promise(() => {}));
    const local = mount(vi.fn().mockResolvedValue(script), { getDynamicWorkflow });
    let sheet = await openRun();
    expect(within(sheet).getByTestId("run-what").textContent).toBe("Script a1b2c3d4");
    fireEvent.click(within(sheet).getByRole("button", { name: "Promote" }));
    await waitFor(() =>
      expect(getDynamicWorkflow).toHaveBeenCalledWith(running.address),
    );
    local.view.unmount();

    mount(vi.fn().mockResolvedValue(script), {}, row({ nodeDid: "did:test:remote" }), {
      nodeDid: "did:test:remote",
      source: "enrollment",
    });
    sheet = await openRun();
    expect(within(sheet).queryByRole("button", { name: "Promote" })).toBeNull();
  });

  it("does not offer Promote on a completed installed run", async () => {
    mount(
      vi
        .fn()
        .mockResolvedValue(
          run({ state: "completed", completed_at: running.created_at + 1_000 }),
        ),
    );
    const sheet = await openRun();
    expect(within(sheet).queryByRole("button", { name: "Promote" })).toBeNull();
  });

  it("opens the run this one resumed from, and goes back", async () => {
    const getJobView = vi.fn(async (request: { rowDocId?: string }) =>
      request.rowDocId === prior.row_doc_id ? prior : running,
    );
    mount(getJobView);
    const sheet = await openRun();
    fireEvent.click(
      within(sheet).getByRole("button", { name: "Resumed from an earlier run" }),
    );
    await waitFor(() =>
      expect(within(sheet).getByTestId("job-state").textContent).toBe("failed"),
    );
    expect(getJobView).toHaveBeenLastCalledWith({
      nodeDid: NODE,
      rowDocId: prior.row_doc_id,
    });
    expect(within(sheet).getByTestId("run-outcome").textContent).toContain(
      "plugin_job_budget",
    );
    expect(
      within(sheet).queryByRole("button", { name: "Resumed from an earlier run" }),
    ).toBeNull();
    fireEvent.click(within(sheet).getByRole("button", { name: "Back" }));
    await waitFor(() =>
      expect(within(sheet).getByTestId("job-state").textContent).toBe("running"),
    );
  });

  it("re-reads when the session observes the row change, and not otherwise", async () => {
    const getJobView = vi
      .fn()
      .mockResolvedValueOnce(run())
      .mockResolvedValue(
        run({
          progress: {
            ...running.progress!,
            last_event: "agent 3 started",
            revision: 4,
          },
        }),
      );
    const { view } = mount(getJobView);
    const sheet = await openRun();
    expect(getJobView).toHaveBeenCalledTimes(1);
    view.rerender(cell(row()));
    expect(getJobView).toHaveBeenCalledTimes(1);
    await act(async () => {
      view.rerender(cell(row({ jobProgress: { ...running.progress!, revision: 4 } })));
    });
    await waitFor(() =>
      expect(within(sheet).getByTestId("job-last-event").textContent).toBe(
        "agent 3 started",
      ),
    );
    expect(getJobView).toHaveBeenCalledTimes(2);
  });

  it("says why a run cannot be read", async () => {
    mount(
      vi.fn().mockRejectedValue(new Error("This run is not visible on the node yet.")),
    );
    fireEvent.click(screen.getByRole("button", { name: "Open run" }));
    const sheet = await screen.findByRole("dialog");
    expect((await within(sheet).findByTestId("run-error")).textContent).toBe(
      "This run is not visible on the node yet.",
    );
  });

  it("offers Open run only once the row has physical identity", () => {
    mount(vi.fn(), {}, row({ toolCallDocId: null }));
    expect(screen.queryByRole("button", { name: "Open run" })).toBeNull();
  });
});
