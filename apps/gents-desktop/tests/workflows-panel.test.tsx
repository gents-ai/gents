import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { DynamicWorkflowSummary } from "@source-inc/gents-desktop-client";
import type { JobView } from "@source-inc/gents-desktop-client/generated/JobView";

import { sessionDraftKey } from "../src/hooks/shellView";
import { href } from "../src/ui/lib/router";
import { WorkflowsPanel } from "../src/ui/screens/agent/WorkflowsPanel";
import { node, publish, renderIn, testApp } from "./app-fixture";
import { harnessWorkflowRuns } from "./ui-harness/desktopHarness";

const NODE = "did:key:z6MkAgent";
const [running] = harnessWorkflowRuns();
const runOn = (over: Partial<JobView> = {}): JobView => ({
  ...running,
  scope: { ...running.scope, node_did: NODE },
  ...over,
});
const sessionsOf = (title: string) => [
  { sessionId: "session-intro", nodeDid: NODE, requesterDid: null, title },
];
const noRuns = () => vi.fn().mockResolvedValue([]);

const summary = (over: Partial<DynamicWorkflowSummary>): DynamicWorkflowSummary => ({
  script_id: "sha256:a",
  handle: "rome_debate",
  runs: 3,
  last_run_at: 1_760_000_000_000,
  agent_did: "agent-1",
  latest_address: "session-1:call-1",
  promoted_as: [],
  ...over,
});

describe("WorkflowsPanel", () => {
  it("renders one row per group with its runs and every promoted name", async () => {
    const listDynamicWorkflows = vi.fn().mockResolvedValue({
      workflows: [
        summary({}),
        summary({
          script_id: "sha256:b",
          handle: "triage",
          runs: 1,
          promoted_as: ["gents/triage", "gents/triage_v2"],
        }),
      ],
      installed: [
        {
          plugin: "gents/review_changes",
          version: "2.0.0",
          description: "Review a diff through four lenses.",
          pack: "gents/code_review",
        },
      ],
    });
    renderIn(
      testApp({ api: { listDynamicWorkflows, listJobs: noRuns() } }),
      <WorkflowsPanel deployment={node()} />,
    );
    const rows = await screen.findAllByTestId("workflow-row");
    expect(rows).toHaveLength(2);
    expect(rows[0]!.textContent).toContain("rome_debate");
    expect(rows[0]!.textContent).toContain("3 runs");
    expect(rows[0]!.textContent).toContain("agent-1");
    expect(rows[1]!.textContent).toContain("1 run");
    expect(within(rows[0]!).getByRole("button", { name: /Promote/ })).toBeTruthy();
    expect(within(rows[1]!).queryByRole("button", { name: /Promote/ })).toBeNull();
    const installed = screen.getAllByTestId("installed-workflow-row");
    expect(installed).toHaveLength(1);
    expect(installed[0]!.textContent).toContain("gents/review_changes");
    expect(installed[0]!.textContent).toContain("gents/code_review");
    expect(within(installed[0]!).queryByRole("button")).toBeNull();
    expect(rows[1]!.textContent).toContain("promoted as gents/triage, gents/triage_v2");
  });

  it("shows a remote node's running runs, but not this desktop's workflows", async () => {
    const listDynamicWorkflows = vi.fn();
    const listJobs = noRuns();
    renderIn(
      testApp({ api: { listDynamicWorkflows, listJobs } }),
      <WorkflowsPanel
        deployment={node({ nodeDid: "did:test:remote", source: "enrollment" })}
      />,
    );
    expect((await screen.findByTestId("running-empty")).textContent).toBe(
      "Nothing running.",
    );
    expect(listJobs).toHaveBeenCalledWith({
      nodeDid: "did:test:remote",
      states: ["running", "paused", "awaiting_user"],
    });
    expect(listDynamicWorkflows).not.toHaveBeenCalled();
    expect(screen.queryByText("Dynamic workflows")).toBeNull();
    expect(screen.queryByText(/No workflows yet/)).toBeNull();
  });

  it("lists every run that is running, paused or waiting, across sessions", async () => {
    const listJobs = vi.fn().mockResolvedValue([
      runOn(),
      runOn({
        row_doc_id: "doc-waiting",
        session_id: "session-other",
        state: "awaiting_user",
        progress: null,
        handle: "triage",
      }),
    ]);
    renderIn(
      testApp({
        api: { listDynamicWorkflows: vi.fn(), listJobs },
        deployments: [node({ nodeDid: NODE, sessions: sessionsOf("Fix the build") })],
      }),
      <WorkflowsPanel deployment={node({ nodeDid: NODE, source: "enrollment" })} />,
    );
    const rows = await screen.findAllByTestId("running-run");
    expect(rows).toHaveLength(2);
    const [first, second] = rows as [HTMLElement, HTMLElement];
    expect(within(first).getByRole("button", { name: "Review the diff" })).toBeTruthy();
    expect(within(first).getByTestId("job-state").textContent).toBe("running");
    expect(first.textContent).toContain("review · 2 of 4 agents · started");
    const session = within(first).getByRole("link", { name: "Fix the build" });
    expect(session.getAttribute("href")).toBe(
      href({ name: "session", sessionId: "session-intro", nodeDid: NODE }),
    );
    /* no progress: the name falls back to the handle and no phase or agents show */
    expect(within(second).getByRole("button", { name: "triage" })).toBeTruthy();
    expect(within(second).getByTestId("job-state").textContent).toBe("awaiting user");
    expect(second.textContent).not.toContain("agents");
    expect(within(second).getByRole("link", { name: "Untitled session" })).toBeTruthy();
  });

  it("opens a running run's sheet from its row", async () => {
    const getJobView = vi.fn().mockResolvedValue(runOn());
    renderIn(
      testApp({
        api: {
          listDynamicWorkflows: vi.fn(),
          listJobs: vi.fn().mockResolvedValue([runOn()]),
          getJobView,
        },
      }),
      <WorkflowsPanel deployment={node({ nodeDid: NODE, source: "enrollment" })} />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Review the diff" }));
    const sheet = await screen.findByRole("dialog");
    expect(await within(sheet).findByTestId("run-what")).toBeTruthy();
    expect(getJobView).toHaveBeenCalledWith({
      nodeDid: NODE,
      rowDocId: running.row_doc_id,
    });
  });

  it("answers a waiting run in its own session's draft", async () => {
    const waiting = runOn({
      state: "awaiting_user",
      progress: { ...running.progress!, question: "Which branch?" },
    });
    const app = testApp({
      api: {
        listDynamicWorkflows: vi.fn(),
        listJobs: vi.fn().mockResolvedValue([waiting]),
        getJobView: vi.fn().mockResolvedValue(waiting),
      },
      selection: { nodeDid: NODE, sessionId: null },
    });
    renderIn(
      app,
      <WorkflowsPanel deployment={node({ nodeDid: NODE, source: "enrollment" })} />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Review the diff" }));
    const sheet = await screen.findByRole("dialog");
    fireEvent.click(await within(sheet).findByRole("button", { name: "Answer" }));
    const key = sessionDraftKey(NODE, "session-intro");
    expect(app.drafts.getState().drafts[key]).toBe("Answer for review: ");
    expect(app.drafts.getState().focus).toBe(key);
  });

  it("reads again when the node's sessions change, and not otherwise", async () => {
    const listJobs = noRuns();
    const app = testApp({
      api: { listDynamicWorkflows: vi.fn(), listJobs },
      deployments: [node({ nodeDid: NODE, sessions: sessionsOf("one") })],
    });
    const view = renderIn(
      app,
      <WorkflowsPanel deployment={node({ nodeDid: NODE, source: "enrollment" })} />,
    );
    await screen.findByTestId("running-empty");
    expect(listJobs).toHaveBeenCalledTimes(1);
    view.rerender(
      <WorkflowsPanel deployment={node({ nodeDid: NODE, source: "enrollment" })} />,
    );
    expect(listJobs).toHaveBeenCalledTimes(1);
    await act(async () => {
      publish(app, [node({ nodeDid: NODE, sessions: sessionsOf("two") })]);
    });
    await waitFor(() => expect(listJobs).toHaveBeenCalledTimes(2));
  });
});
