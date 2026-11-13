import { cleanup, renderHook, screen, within } from "@testing-library/react";
import type { ReactElement } from "react";
import { afterEach, describe, expect, it } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { NavNodes } from "../src/ui/app/NavNodes";
import { resetListViews } from "../src/ui/app/listViews";
import { useNodeConfigReadOnly } from "../src/ui/hooks/useClient";
import { pageConnection } from "../src/ui/lib/nodeConnection";
import { AutomationsScreen } from "../src/ui/screens/AutomationsScreen";
import { MailboxScreen } from "../src/ui/screens/MailboxScreen";
import { SessionsScreen } from "../src/ui/screens/SessionsScreen";
import { AgentsPanel } from "../src/ui/screens/agent/BehaviorsPanel";
import type { NodeView } from "../src/hooks/fleetStore";
import { node, renderIn, testApp, withApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const FORGE = "did:key:forge";

const session: SessionSummary = {
  sessionId: "s1",
  nodeDid: FORGE,
  requesterDid: null,
  startedBy: null,
  latestRequestDocId: "physical-1",
  latestRequestId: "logical-1",
  closedAt: null,
  archivedAt: null,
  archiveRevision: "0",
  tags: [],
  provenance: null,
  title: "Greeting",
  previewText: null,
  status: "completed",
  agentDid: null,
  taskId: null,
  taskName: null,
  triggerId: null,
  triggerKind: null,
  createdAt: null,
  updatedAt: null,
  turnState: "idle",
  messageCount: null,
  toolCallCount: null,
};

const forge = (runtimeConnection: string, withData: boolean) =>
  node({
    nodeDid: FORGE,
    label: "Forge",
    source: "enrollment",

    dialSucceeded: true,
    runtimeConnection,
    node: { ...deployment.node, nodeDid: FORGE, displayName: "Forge" },
    agents: withData ? deployment.agents.map((a) => ({ ...a, nodeDid: FORGE })) : [],
    triggers: withData ? deployment.triggers : [],
    tasks: withData ? deployment.tasks : [],
    sessions: withData ? [session] : [],
    mailboxItems: withData
      ? [
          {
            itemId: "item-1",
            itemKey: "key-1",
            requesterDid: "did:key:person",
            nodeDid: FORGE,
            status: "open",
            kind: "finished",
            action: "ack",
            title: "Refactor landed",
            summary: null,
            payload: null,
            sourceKind: "session",
            sourceId: "s1",
            sessionId: "s1",
            requestId: null,
            causeDocId: null,
            targetNodeDid: FORGE,
            targetAgentDid: "default",
            expectedCollection: null,
            parentItemId: null,
            deadlineAt: null,
            createdAt: new Date().toISOString(),
          },
        ]
      : [],
  });

const appOn = (deployments: unknown[]) =>
  testApp({
    snapshot: {
      bootstrap: { initNodeDid: deployment.nodeDid },
      client: { deployments },
    },
    selection: { nodeDid: FORGE },
  });

afterEach(() => {
  cleanup();
  resetListViews();
});

describe("a node's connection", () => {
  it("decides what a page of the node's data shows", () => {
    const at = (connection: string, withData: boolean) => {
      const shown = pageConnection(forge(connection, withData) as NodeView, withData);
      return shown && { kind: shown.kind, reason: shown.status.reason };
    };
    expect(at("connected", false)).toBeNull();
    expect(at("connecting", true)).toBeNull();
    expect(at("connecting", false)).toEqual({ kind: "loading", reason: "connecting" });
    expect(at("refused", false)).toEqual({ kind: "unavailable", reason: "refused" });
    expect(at("noEndpoint", false)).toEqual({
      kind: "unavailable",
      reason: "no_endpoint",
    });
    expect(at("disconnected", false)).toEqual({
      kind: "unavailable",
      reason: "disconnected",
    });
    expect(at("disconnected", true)).toEqual({ kind: "stale", reason: "disconnected" });
  });

  it.each([
    ["connecting", "Connecting"],

    ["refused", "Refused"],
  ])("names itself beside the node in the Sidebar: %s", (connection, label) => {
    renderIn(
      appOn([node(), forge(connection, false)]),
      <TooltipProvider>
        <NavNodes route={{ name: "mailbox" }} current={deployment.nodeDid} />
      </TooltipProvider>,
    );
    expect(
      within(screen.getByRole("button", { name: /Forge/ })).getByText(label),
    ).toBeVisible();
  });

  it("says a remote node's settings are read-only because it isn't connected", () => {
    const app = appOn([node(), forge("disconnected", true)]);
    const { result } = renderHook(
      () => useNodeConfigReadOnly(forge("disconnected", true) as NodeView),
      { wrapper: withApp(app) },
    );
    expect(result.current).toBe("Read-only here: Can’t reach Forge.");
  });
});

const states = (
  page: (forge: NodeView) => ReactElement,
  empty: string,
  shown: string,
) => {
  const show = (connection: string, withData: boolean) => {
    const at = forge(connection, withData);
    renderIn(appOn([node(), at]), page(at as NodeView));
  };

  it("loads while its node connects, without saying it's empty", () => {
    show("connecting", false);
    expect(screen.getByText("Connecting to Forge…")).toBeVisible();
    expect(screen.queryByText(empty, { exact: false })).toBeNull();
  });

  it("says its node refuses this app, without saying it's empty", () => {
    show("refused", false);
    expect(screen.getByText("Forge doesn’t accept this app")).toBeVisible();
    expect(screen.getByText(/restore this device's access/)).toBeVisible();
    expect(screen.queryByText(empty, { exact: false })).toBeNull();
  });

  it("keeps what a lost connection left, marked as possibly out of date", () => {
    show("disconnected", true);
    expect(screen.getByText(shown)).toBeVisible();
    expect(screen.getByTestId("node-connection-stale")).toHaveTextContent(
      "Can’t reach Forge. This may be out of date.",
    );
  });
};

describe("the Sessions page", () => {
  states(() => <SessionsScreen />, "No sessions yet", "Greeting");
});

describe("the Mailbox", () => {
  states(() => <MailboxScreen />, "Nothing needs your attention", "Refactor landed");
});

describe("the Automations page", () => {
  states(() => <AutomationsScreen />, "Automate work on", "Task B");
});

describe("a node's agents", () => {
  states((at) => <AgentsPanel deployment={at} />, "No agents yet", "Ops");
});
