import { screen } from "@testing-library/react";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { expect, it } from "vitest";

import { NavPanel } from "../src/ui/app/RailFlyout";
import { node, renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

it("shows the active remote destination and opens its configuration", () => {
  const remote = node({
    nodeDid: "did:key:remote",
    source: "peer",
    label: "studio-1",
    node: { ...deployment.node, nodeDid: "did:key:remote", displayName: "Amy" },
  });
  const app = testApp({
    deployments: [node(), remote],
    selection: { nodeDid: "did:key:remote" },
  });
  renderIn(
    app,
    <TooltipProvider>
      <NavPanel route={{ name: "sessions" }} settings={null} syncRow={false} />
    </TooltipProvider>,
  );
  const active = screen.getByTestId("working-node");
  expect(active).toHaveTextContent("Amy");
  expect(active).toHaveTextContent("Remote node");
  expect(active).toHaveAttribute("href", "/agents/did%3Akey%3Aremote/agent");
  expect(active).toHaveAttribute("title", expect.stringContaining("did:key:remote"));
});
