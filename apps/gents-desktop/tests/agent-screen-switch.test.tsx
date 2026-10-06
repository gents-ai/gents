import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { describe, expect, it } from "vitest";

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
