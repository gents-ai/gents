import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { workspace } from "@/app/workspace";
import { StartedSessionsButton } from "../src/ui/screens/StartedSessionsButton";
import { node, testApp, withApp } from "./app-fixture";

const session = (overrides: Partial<SessionSummary>): SessionSummary => ({
  sessionId: "parent",
  nodeDid: "did:test:owner",
  requesterDid: null,
  startedBy: null,
  latestRequestDocId: "physical-parent-1",
  latestRequestId: "logical-parent-1",
  closedAt: null,
  archivedAt: null,
  archiveRevision: "0",
  tags: [],
  provenance: null,
  title: "Lead",
  previewText: null,
  status: "processing",
  agentId: null,
  taskId: null,
  taskName: null,
  triggerId: null,
  triggerKind: null,
  createdAt: null,
  updatedAt: null,
  turnState: "running",
  messageCount: null,
  toolCallCount: null,
  ...overrides,
});

const worker = (sessionId: string, turnState: string) =>
  session({
    sessionId,
    title: sessionId,
    latestRequestDocId: `physical-${sessionId}`,
    latestRequestId: `logical-${sessionId}`,
    turnState,
    startedBy: {
      sessionId: "parent",
      nodeDid: "did:test:owner",
      requesterDid: null,
      causeRequestDocId: "physical-parent-1",
    },
  });

const renderWith = (sessions: SessionSummary[]) =>
  render(<StartedSessionsButton sessionId="parent" />, {
    wrapper: withApp(
      testApp({ deployments: [node({ nodeDid: "did:key:node", sessions })] }),
    ),
  });

describe("the started sessions button", () => {
  beforeEach(() => workspace.reset());

  it("counts the sessions this one started and says how many are running", () => {
    renderWith([
      session({}),
      worker("a", "running"),
      worker("b", "completed"),
      worker("c", "completed"),
    ]);
    const button = screen.getByRole("button", {
      name: "3 started sessions, 1 running",
    });
    expect(button).toHaveTextContent("3");
    expect(screen.getByTestId("started-sessions-running")).toBeInTheDocument();
  });

  it("is not marked as running once its sessions are done", () => {
    renderWith([session({}), worker("a", "completed")]);
    expect(
      screen.getByRole("button", { name: "1 started session" }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("started-sessions-running")).toBeNull();
  });

  it("is absent when the session started none", () => {
    renderWith([session({})]);
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("shows them in the dock, and hides them again", () => {
    renderWith([session({}), worker("a", "completed")]);
    const button = screen.getByRole("button", { name: "1 started session" });
    fireEvent.click(button);
    expect(workspace.dock("session:parent")).toMatchObject({
      open: true,
      active: "workers",
    });
    expect(button).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(button);
    expect(workspace.dock("session:parent").tabs).not.toContain("workers");
    expect(button).toHaveAttribute("aria-pressed", "false");
  });
});
