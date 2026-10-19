import { screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AgentCard } from "../src/ui/screens/agent/AgentCard";
import { renderIn, testApp } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

function cardWith(backgroundedTools: object[]) {
  const app = testApp({
    api: { fetchOperationsSnapshot: vi.fn().mockResolvedValue({ backgroundedTools }) },
    snapshot: { bootstrap, client: { deployments: [deployment] } },
  });
  renderIn(app, <AgentCard deployment={deployment} />);
}

describe("AgentCard background labels", () => {
  it("labels a job row as a workflow", async () => {
    cardWith([
      { toolCallId: "call-run", toolName: "workflow", startsSession: false, job: true },
    ]);
    expect(await screen.findByText(/1 workflow$/)).toBeVisible();
    expect(screen.queryByText(/background job/)).toBeNull();
  });

  it("keeps the background label for a plain background row", async () => {
    cardWith([
      { toolCallId: "call-job", toolName: "shell", startsSession: false, job: false },
      { toolCallId: "call-run", toolName: "workflow", startsSession: false, job: true },
    ]);
    expect(await screen.findByText(/1 workflow · 1 background job$/)).toBeVisible();
  });
});
