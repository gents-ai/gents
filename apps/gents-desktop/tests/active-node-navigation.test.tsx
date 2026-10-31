import { screen } from "@testing-library/react";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { expect, it } from "vitest";

import { Rail } from "../src/ui/app/Rail";
import { NavPanel } from "../src/ui/app/RailFlyout";
import { avatarFor } from "../src/ui/lib/avatar";
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

it("draws the active node in the collapsed rail by its label when it has no name", () => {
  const unnamed = (nodeDid: string, label: string) =>
    node({
      nodeDid,
      source: nodeDid === "did:key:remote" ? "peer" : undefined,
      label,
      node: { ...deployment.node, nodeDid, displayName: null },
    });
  const app = testApp({
    deployments: [
      unnamed("did:key:node", "forge"),
      unnamed("did:key:remote", "studio-1"),
    ],
    selection: { nodeDid: "did:key:remote" },
  });
  renderIn(
    app,
    <TooltipProvider>
      <Rail route={{ name: "sessions" }} settings={null} />
    </TooltipProvider>,
  );
  const active = screen.getByRole("link", { name: "studio-1 configuration" });
  expect(active.querySelector("img")).toHaveAttribute("src", avatarFor("studio-1"));
});
