import { screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { DynamicWorkflowSummary } from "@source-inc/gents-desktop-client";

import { WorkflowsPanel } from "../src/ui/screens/settings/WorkflowsPanel";
import { renderIn, testApp } from "./app-fixture";

const summary = (over: Partial<DynamicWorkflowSummary>): DynamicWorkflowSummary => ({
  script_id: "sha256:a",
  handle: "rome_debate",
  runs: 3,
  first_run_at: 1,
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
    });
    renderIn(testApp({ api: { listDynamicWorkflows } }), <WorkflowsPanel />);
    const rows = await screen.findAllByTestId("workflow-row");
    expect(rows).toHaveLength(2);
    expect(rows[0]!.textContent).toContain("rome_debate");
    expect(rows[0]!.textContent).toContain("3 runs");
    expect(rows[0]!.textContent).toContain("agent-1");
    expect(rows[1]!.textContent).toContain("1 run");
    expect(rows[1]!.textContent).toContain("promoted as gents/triage, gents/triage_v2");
    expect(within(rows[1]!).getByRole("button", { name: "Promote…" })).toBeTruthy();
  });
});
