import { act, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { describe, expect, it, vi } from "vitest";

import { AgentScreen } from "../src/ui/screens/agent/AgentScreen";
import { node, renderIn, testApp } from "./app-fixture";

describe("switching agents on the configuration screen", () => {
  it("drops a draft opened on the previous agent", async () => {
    const app = testApp({
      deployments: [node({ agentDid: "did:key:a" }), node({ agentDid: "did:key:b" })],
    });
    const view = renderIn(
      app,
      <TooltipProvider>
        <AgentScreen agentDid="did:key:a" section="behaviors" />
      </TooltipProvider>,
    );
    await userEvent.click(screen.getByRole("button", { name: "New behavior" }));
    expect(screen.getByRole("button", { name: "Behaviors" })).toBeInTheDocument();

    act(() =>
      view.rerender(
        <TooltipProvider>
          <AgentScreen agentDid="did:key:b" section="behaviors" />
        </TooltipProvider>,
      ),
    );
    expect(screen.queryByRole("button", { name: "Behaviors" })).toBeNull();
    expect(screen.getByRole("button", { name: "New behavior" })).toBeInTheDocument();
  });
});

describe("the inference route", () => {
  it("lists what the Providers page lists, profiles under their backends", async () => {
    const app = testApp({ deployments: [node({ agentDid: "did:key:a" })] });
    renderIn(
      app,
      <TooltipProvider>
        <AgentScreen agentDid="did:key:a" section="inference" />
      </TooltipProvider>,
    );
    const [toggle] = screen.getAllByRole("button", { name: /^Show what .* serves$/ });
    await userEvent.click(toggle!);
    expect(screen.getAllByText("Add profile").length).toBeGreaterThan(0);
  });
});

describe("turning a task off from its row", () => {
  it("patches only enabled, leaving the rest of the task as stored", async () => {
    const patchConfigComponents = vi.fn().mockResolvedValue(undefined);
    const saveTaskConfig = vi.fn().mockResolvedValue(undefined);
    const app = testApp({
      api: { patchConfigComponents, saveTaskConfig, fetchClientSnapshot: vi.fn() },
      deployments: [node({ agentDid: "did:key:a" })],
    });
    renderIn(
      app,
      <TooltipProvider>
        <AgentScreen agentDid="did:key:a" section="tasks" />
      </TooltipProvider>,
    );
    await userEvent.click(screen.getByRole("switch", { name: "Task A is on" }));
    await waitFor(() => expect(patchConfigComponents).toHaveBeenCalledOnce());
    expect(patchConfigComponents).toHaveBeenCalledWith({
      agentDid: "did:key:a",
      patches: [{ collection: "Task", id: "task-a", changes: { enabled: false } }],
    });
    expect(saveTaskConfig).not.toHaveBeenCalled();
  });
});
