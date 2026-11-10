import { screen, within } from "@testing-library/react";
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

it("names the number of nodes quietly, and only once there is more than one", () => {
  const nodesRow = () =>
    screen.getAllByRole("link").find((a) => a.textContent?.startsWith("Nodes"))!;
  const { unmount } = renderIn(
    testApp({ deployments: [node()] }),
    <TooltipProvider>
      <NavPanel route={{ name: "sessions" }} settings={null} syncRow={false} />
    </TooltipProvider>,
  );
  expect(nodesRow().textContent).not.toMatch(/\d$/);
  unmount();

  const other = node({
    nodeDid: "did:key:other",
    source: "peer",
    node: { ...deployment.node, nodeDid: "did:key:other", displayName: "Amy" },
  });
  renderIn(
    testApp({ deployments: [node(), other] }),
    <TooltipProvider>
      <NavPanel route={{ name: "sessions" }} settings={null} syncRow={false} />
    </TooltipProvider>,
  );
  const tally = within(nodesRow()).getByText("2");
  /* a fact, not something waiting: no brand badge */
  expect(tally).not.toHaveClass("bg-brand");
  expect(tally).toHaveClass("text-muted-foreground");
});
