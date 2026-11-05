import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryNavProvider } from "@gents/shell";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";

import { SessionScreen } from "../src/ui/screens/SessionScreen";
import type { DesktopApp } from "../src/hooks/desktopApp";
import { node, publish, testApp, withApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

vi.mock("../src/ui/screens/BehaviorPicker", () => ({
  AgentPicker: () => <button type="button">Agent</button>,
}));

beforeEach(() => {
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
});

const NODE = deployment.nodeDid;

const readiness = (reason: string | null) => ({
  ...deployment.nodeReadiness,
  agents: [
    reason
      ? { state: "unavailable", agentDid: "default", reason }
      : { state: "ready", agentDid: "default" },
    { state: "ready", agentDid: "ops" },
  ],
});

function show(
  overrides: Record<string, unknown>,
  session: DesktopSessionSnapshot | null = null,
) {
  const app = testApp({
    api: {
      sessionProvenance: vi.fn().mockResolvedValue(null),
      fetchOperationsSnapshot: vi.fn().mockResolvedValue(null),
    },
    deployments: [node(overrides)],
    session,
    selection: { nodeDid: NODE, sessionId: session?.sessionId ?? null },
  });
  render(
    <MemoryNavProvider
      initial={{ name: "session", sessionId: session?.sessionId ?? null }}
    >
      <SessionScreen />
    </MemoryNavProvider>,
    { wrapper: withApp(app) },
  );
  return app;
}

/* the input stays beneath, out of reach, so the page keeps its height */
const inputOutOfReach = () =>
  expect(
    screen.getByRole("textbox", { name: "Message" }).closest("[inert]"),
  ).not.toBeNull();

const box = () => screen.getByTestId("composer-reason");

describe("the composer's reason box", () => {
  it("says a node has no inference, and leads to setting it up", () => {
    show({ nodeReadiness: readiness("backend_not_configured") });
    expect(box()).toHaveTextContent(/has no inference set up/);
    expect(screen.getByRole("link", { name: "Set up inference" })).toHaveAttribute(
      "href",
      expect.stringContaining("/inference"),
    );
    expect(screen.getByRole("link", { name: "Switch node" })).toBeInTheDocument();
    /* the agent can still be changed, from the box */
    expect(box()).toContainElement(screen.getByRole("button", { name: "Agent" }));
    inputOutOfReach();
  });

  it("sends a remote node's set-up to that machine", () => {
    show({
      source: "enrollment",
      graphql: null,
      nodeReadiness: readiness("backend_not_configured"),
    });
    expect(box()).toHaveTextContent("Set it up on that machine");
    expect(screen.queryByRole("link", { name: "Set up inference" })).toBeNull();
  });

  it("says a node is offline", () => {
    show({ runtimeConnection: "disconnected" });
    expect(box()).toHaveTextContent(/is offline/);
    expect(screen.getByRole("link", { name: "Switch node" })).toBeInTheDocument();
    inputOutOfReach();
  });

  it("gives the reason a session takes no messages from here", () => {
    const reason = "Read-only. Message the session that started it.";
    show(
      {
        sessions: [
          {
            sessionId: "worker",
            nodeDid: NODE,
            requesterDid: NODE,
            turnState: "completed",
            latestRequestId: "request",
            unsendableReason: reason,
          },
        ],
      },
      {
        sessionId: "worker",
        nodeDid: NODE,
        agentDid: "default",
        title: "Worker",
        turnState: "completed",
        timelineItems: [],
        timelinePage: { hasOlder: false },
        latestRequestId: "request",
        latestRequestOutcome: null,
        goal: null,
        context: null,
        queuedTurns: [],
        foldedInputs: [],
      } as unknown as DesktopSessionSnapshot,
    );
    expect(box()).toHaveTextContent(reason);
    inputOutOfReach();
  });

  it("leaves the input for an outage that passes, and goes once the cause clears", () => {
    const app: DesktopApp = show({
      nodeReadiness: readiness("backend_temporarily_unavailable"),
    });
    expect(screen.queryByTestId("composer-reason")).toBeNull();

    act(() =>
      publish(app, [node({ nodeReadiness: readiness("backend_not_configured") })]),
    );
    expect(box()).toBeInTheDocument();
    act(() => publish(app, [node({ nodeReadiness: readiness(null) })]));
    expect(screen.queryByTestId("composer-reason")).toBeNull();
    expect(
      screen.getByRole("textbox", { name: "Message" }).closest("[inert]"),
    ).toBeNull();
  });
});
