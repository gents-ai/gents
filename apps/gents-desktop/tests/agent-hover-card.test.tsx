import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { fleetNodes } from "../src/ui/lib/scope";
import { AgentHoverCard } from "../src/ui/screens/HoverCards";
import { node, renderIn, testApp } from "./app-fixture";

/* this machine was set up with /Users/me/work; its node is did:key:home,
   paired by enrollment like any peer */
function cardFor(agentDid: string) {
  const app = testApp({
    snapshot: {
      bootstrap: {
        initAgentDid: "did:key:home",
        initToolRoot: "/Users/me/work",
        initToolCeiling: "readwrite",
      },
      client: { deployments: [node({ agentDid, source: "enrollment" })] },
    },
  });
  const [shown] = fleetNodes(app.stores.fleet.getState());
  renderIn(
    app,
    <AgentHoverCard deployment={shown!}>
      <button type="button">avatar</button>
    </AgentHoverCard>,
  );
}

describe("an agent's hover card", () => {
  it("shows this machine's tool root on the node this machine runs", async () => {
    cardFor("did:key:home");
    await userEvent.hover(screen.getByRole("button", { name: "avatar" }));
    expect(
      await screen.findByText("/Users/me/work", {}, { timeout: 2000 }),
    ).toBeVisible();
  });

  it("does not lend this machine's tool root to a remote node", async () => {
    cardFor("did:key:remote");
    await userEvent.hover(screen.getByRole("button", { name: "avatar" }));
    expect(await screen.findByText("Behaviors", {}, { timeout: 2000 })).toBeVisible();
    expect(screen.queryByText("/Users/me/work")).toBeNull();
  });
});
