import { cleanup, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import type { NodeView } from "../src/hooks/fleetStore";
import { nodeConnectionState } from "../src/ui/lib/nodeConnection";
import { AgentScreen } from "../src/ui/screens/agent/AgentScreen";
import { CONNECTION_GROUP } from "../src/ui/screens/agent/ConnectionGroup";
import { href } from "../src/ui/lib/router";
import { node, renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const HOME = deployment.nodeDid;
const JOINED = "did:key:amy";
const PEER = "12D3KooWAmyTransportPeerId";
const DAY = 24 * 60 * 60 * 1000;

function nodePage(nodeDid: string, overrides: Partial<NodeView> = {}, item?: string) {
  const joined = node({
    nodeDid: JOINED,
    label: "amy-1",
    peerId: PEER,
    addr: "/ip4/10.0.0.7/tcp/9191",
    source: "enrollment",
    operatorAccess: false,
    leaseExpiresAt: new Date(Date.now() + 2 * DAY + 60_000).toISOString(),
    ...overrides,
    node: { ...deployment.node, nodeDid: JOINED, displayName: "Amy" },
  });
  const app = testApp({
    api: { fetchOperationsSnapshot: vi.fn().mockResolvedValue(null) },
    deployments: [node({ peerId: "3f1c2a7e-5b0d-4c49-9a8e-1d2b3c4d5e6f" }), joined],
    selection: { nodeDid },
  });
  renderIn(
    app,
    <TooltipProvider>
      <AgentScreen nodeDid={nodeDid} section="agent" item={item} />
    </TooltipProvider>,
  );
  return within(
    screen.getByText("Connection", { selector: "legend" }).closest("fieldset")!,
  );
}

/* a fact's row in the group, found by its label */
const rowOf = (group: ReturnType<typeof within>, label: string) =>
  group
    .getByText(label, { selector: "label, [data-slot=field-label]" })
    .closest("[data-slot=field-row]") as HTMLElement;

afterEach(cleanup);

describe("a node's Connection group", () => {
  it("says this computer's node is connected, where, and that this app operates it", () => {
    const group = nodePage(HOME);
    const status = within(rowOf(group, "Status"));
    expect(status.getByText("Connected")).toBeVisible();
    expect(
      status.getByText("The authenticated runtime connection is ready."),
    ).toBeVisible();
    expect(within(rowOf(group, "Address")).getByText("iroh://local")).toBeVisible();
    expect(within(rowOf(group, "Node")).getByText("Local node")).toBeVisible();
    expect(within(rowOf(group, "Operator access")).getByText("Yes")).toBeVisible();
    expect(group.queryByText("Access")).toBeNull();
    expect(group.queryByText("Last error")).toBeNull();
  });

  it("shows no peer ID for this computer's node, here or in the header", () => {
    const group = nodePage(HOME);
    expect(group.queryByText("Peer ID")).toBeNull();
    expect(screen.queryByText(/3f1c2a7e|1d2b3c4d5e6f|4d5e6f/)).toBeNull();
    expect(screen.queryByText("Peer")).toBeNull();
  });

  it("shows a joined node's peer ID with a copy button, how long access lasts, and its last error", () => {
    const group = nodePage(JOINED, {
      runtimeConnection: "disconnected",
      lastError: "dial timed out",
    });
    const status = within(rowOf(group, "Status"));
    expect(status.getByText("Reconnecting")).toBeVisible();
    expect(
      status.getByText("The runtime connection was lost; reconnecting."),
    ).toBeVisible();
    expect(
      within(rowOf(group, "Address")).getByText("/ip4/10.0.0.7/tcp/9191"),
    ).toBeVisible();
    expect(within(rowOf(group, "Node")).getByText("Remote node")).toBeVisible();
    const peer = within(rowOf(group, "Peer ID"));
    expect(peer.getByText(PEER)).toBeVisible();
    expect(peer.getByRole("button", { name: "Copy peer ID" })).toBeVisible();
    expect(screen.getByTestId("node-connection-lease")).toHaveTextContent(
      "Access ends in 2 days",
    );
    expect(screen.getByTestId("node-connection-lease")).toHaveClass("text-warning");
    expect(within(rowOf(group, "Operator access")).getByText("No")).toBeVisible();
    expect(
      within(rowOf(group, "Last error")).getByText("dial timed out"),
    ).toBeVisible();
  });

  it("keeps the peer ID to enrollment records", () => {
    const group = nodePage(JOINED, { source: "status" });
    expect(within(rowOf(group, "Node")).getByText("Remote node")).toBeVisible();
    expect(group.queryByText("Peer ID")).toBeNull();
  });
});

describe("a node's connection state", () => {
  const of = (runtimeConnection: string | undefined, stopped = false) => {
    const { label, link } = nodeConnectionState({ runtimeConnection }, stopped);
    return { label, link };
  };

  it("takes its label from the owner and draws each reason's line", () => {
    expect(of("connected")).toEqual({ label: "Connected", link: "connected" });
    expect(of(undefined)).toEqual({ label: "Connecting", link: "connecting" });
    expect(of("disconnected")).toEqual({ label: "Reconnecting", link: "connecting" });
    expect(of("refused")).toEqual({ label: "Refused", link: "refused" });
    expect(of("noEndpoint")).toEqual({ label: "No address", link: "idle" });
  });

  it("reads this computer's stopped node as stopped", () => {
    expect(of("disconnected", true)).toEqual({ label: "Stopped", link: "idle" });
  });
});

/* the node summary's Connection card, by its title */
const card = () => screen.getByRole("link", { name: /^Connection/ });

describe("the node summary's Connection card", () => {
  it("shows this computer's node's state and address", () => {
    nodePage(HOME);
    const c = within(card());
    expect(c.getByText("Connected")).toBeVisible();
    expect(c.getByText("iroh://local")).toHaveClass("font-mono");
  });

  it("warns how soon a joined node's access ends", () => {
    nodePage(JOINED);
    const c = within(card());
    expect(c.getByText("Connected")).toBeVisible();
    expect(c.getByText("Access ends in 2 days")).toHaveClass("text-warning");
    expect(c.queryByText("/ip4/10.0.0.7/tcp/9191")).toBeNull();
  });

  it("says what went wrong when the node refuses this app", () => {
    nodePage(JOINED, {
      runtimeConnection: "refused",
      lastError: "certificate rejected",
    });
    const c = within(card());
    expect(c.getByText("Refused")).toBeVisible();
    expect(c.getByText("certificate rejected")).toBeVisible();
    expect(c.queryByText(/Access ends/)).toBeNull();
  });

  it("falls back to the owner's detail when a problem has no error", () => {
    nodePage(JOINED, { runtimeConnection: "disconnected", lastError: null });
    expect(
      within(card()).getByText("The runtime connection was lost; reconnecting."),
    ).toBeVisible();
  });

  it("opens the node page at its Connection group", async () => {
    const to = href({
      name: "agent",
      nodeDid: JOINED,
      section: "agent",
      item: CONNECTION_GROUP,
    });
    nodePage(JOINED);
    expect(card()).toHaveAttribute("href", to);
    cleanup();
    const scrolled = vi.spyOn(HTMLElement.prototype, "scrollIntoView");
    nodePage(JOINED, {}, CONNECTION_GROUP);
    await waitFor(() =>
      expect(scrolled.mock.contexts).toContain(
        document.getElementById(CONNECTION_GROUP),
      ),
    );
    expect(document.getElementById(CONNECTION_GROUP)?.tagName).toBe("FIELDSET");
    scrolled.mockRestore();
  });
});
