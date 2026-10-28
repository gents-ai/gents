import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { SessionSummary } from "@source-inc/gents-desktop-client";

const router = vi.hoisted(() => ({ navigate: vi.fn() }));
vi.mock("@/lib/router", () => ({ href: () => "#", navigate: router.navigate }));

import { MailboxScreen } from "../src/ui/screens/MailboxScreen";
import { SessionsScreen } from "../src/ui/screens/SessionsScreen";
import { listViews, resetListViews } from "../src/ui/app/listViews";
import { emptyFilter } from "../src/ui/lib/session-filter";
import { node, renderIn, testApp } from "./app-fixture";

const summary = (nodeDid: string, sessionId: string) =>
  ({
    sessionId,
    nodeDid,
    requesterDid: null,
    archivedAt: null,
    agentId: null,
    title: sessionId,
    turnState: null,
    updatedAt: null,
  }) as SessionSummary;

const app = () =>
  testApp({
    deployments: [
      node({ nodeDid: "did:key:here", sessions: [summary("did:key:here", "a")] }),
      node({ nodeDid: "did:key:there", sessions: [summary("did:key:there", "b")] }),
    ],
  });

afterEach(resetListViews);

describe("clearing list content filters", () => {
  it("keeps the global active node in the sessions list", async () => {
    router.navigate.mockClear();
    const client = app();
    client.actions.selectNode("did:key:there");
    listViews.setSessionFilter({ ...emptyFilter, agents: ["missing"] });
    renderIn(client, <SessionsScreen />);
    await userEvent.click(screen.getByRole("button", { name: "Clear filters" }));
    expect(client.stores.selection.getState().nodeDid).toBe("did:key:there");
    expect(screen.getByText("b")).toBeInTheDocument();
    expect(screen.queryByText("a")).toBeNull();
    expect(router.navigate).not.toHaveBeenCalled();
  });

  it("keeps the global active node in the mailbox", async () => {
    router.navigate.mockClear();
    const client = app();
    client.actions.selectNode("did:key:there");
    listViews.setMailboxKinds(["failed"]);
    renderIn(client, <MailboxScreen />);
    await userEvent.click(screen.getByRole("button", { name: "Clear filters" }));
    expect(client.stores.selection.getState().nodeDid).toBe("did:key:there");
    expect(router.navigate).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "Clear filters" })).toBeNull();
  });
});
