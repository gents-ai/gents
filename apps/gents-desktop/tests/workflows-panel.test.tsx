import { screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { DynamicWorkflowSummary } from "@source-inc/gents-desktop-client";

import { WorkflowsPanel } from "../src/ui/screens/agent/WorkflowsPanel";
import { node, renderIn, testApp } from "./app-fixture";

const summary = (over: Partial<DynamicWorkflowSummary>): DynamicWorkflowSummary => ({
  script_id: "sha256:a",
  handle: "rome_debate",
  runs: 3,
  last_run_at: 1_760_000_000_000,
  agent_id: "agent-1",
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
      testApp({ api: { listDynamicWorkflows } }),
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

  it("does not query or show local workflows while viewing a remote node", () => {
    const listDynamicWorkflows = vi.fn();
    const { container } = renderIn(
      testApp({ api: { listDynamicWorkflows } }),
      <WorkflowsPanel
        deployment={node({ nodeDid: "did:test:remote", source: "enrollment" })}
      />,
    );
    expect(container).toBeEmptyDOMElement();
    expect(listDynamicWorkflows).not.toHaveBeenCalled();
  });
});
