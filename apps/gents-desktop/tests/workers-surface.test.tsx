import { fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { workspace } from "@/app/workspace";
import { WorkersSurface } from "../src/ui/screens/WorkersSurface";
import { node, testApp, withApp } from "./app-fixture";

const session = (overrides: Partial<SessionSummary>): SessionSummary => ({
  sessionId: "parent",
  nodeDid: "did:test:owner",
  requesterDid: null,
  startedBy: null,
  latestRequestDocId: "physical-parent",
  latestRequestId: "logical-parent",
  closedAt: null,
  tags: [],
  provenance: null,
  title: "Lead",
  previewText: null,
  status: null,
  agentId: null,
  taskId: null,
  taskName: null,
  triggerId: null,
  triggerKind: null,
  createdAt: null,
  updatedAt: null,
  turnState: "completed",
  messageCount: null,
  toolCallCount: null,
  ...overrides,
});

const startedBy = (sessionId: string) => ({
  sessionId,
  nodeDid: "did:test:owner",
  requesterDid: null,
  causeRequestDocId: `physical-${sessionId}`,
});

const worker = (sessionId: string, overrides: Partial<SessionSummary>) =>
  session({
    sessionId,
    title: sessionId,
    latestRequestDocId: `physical-${sessionId}`,
    latestRequestId: `logical-${sessionId}`,
    startedBy: startedBy("parent"),
    ...overrides,
  });

describe("the started sessions dock", () => {
  beforeEach(() => workspace.reset());

  it("lists started sessions newest first, and shows what one started in place", () => {
    const sessions = [
      session({}),
      worker("older", {
        createdAt: "2026-10-20T10:00:00Z",
        updatedAt: "2026-10-20T10:02:00Z",
      }),
      worker("newer", { createdAt: "2026-10-21T10:00:00Z", turnState: "failed" }),
      worker("busy", { createdAt: "2026-10-19T10:00:00Z", turnState: "running" }),
      worker("grandchild", { turnState: "running", startedBy: startedBy("older") }),
    ];
    render(<WorkersSurface placement="dock" sessionId="parent" />, {
      wrapper: withApp(
        testApp({ deployments: [node({ nodeDid: "did:key:node", sessions })] }),
      ),
    });
    const rows = screen.getAllByRole("listitem");
    expect(rows.map((row) => within(row).getAllByRole("link")[0]!.textContent)).toEqual(
      ["newer", "older", "busy"],
    );
    expect(within(rows[0]!).getByRole("img", { name: "Failed" })).toBeInTheDocument();
    expect(rows[1]).toHaveTextContent("2m");
    expect(rows[2]).toHaveTextContent(/\d+[mhd]/);
    expect(screen.queryByRole("link", { name: "grandchild" })).toBeNull();
    fireEvent.click(
      within(rows[1]!).getByRole("button", {
        name: "Show 1 started session · 1 running",
      }),
    );
    const nested = within(rows[1]!).getAllByRole("listitem");
    expect(
      nested.map((row) => within(row).getAllByRole("link")[0]!.textContent),
    ).toEqual(["grandchild"]);
    expect(
      within(rows[1]!).getByRole("button", {
        name: "Hide 1 started session · 1 running",
      }),
    ).toHaveAttribute("aria-expanded", "true");
  });
});
