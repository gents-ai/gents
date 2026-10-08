import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { SessionSummary } from "@source-inc/gents-desktop-client";

const router = vi.hoisted(() => ({ navigate: vi.fn() }));
vi.mock("@/lib/router", () => ({ href: () => "#", navigate: router.navigate }));

import { MailboxScreen } from "../src/ui/screens/MailboxScreen";
import { SessionsScreen } from "../src/ui/screens/SessionsScreen";
import { node, renderIn, testApp } from "./app-fixture";

const summary = (agentDid: string, sessionId: string) =>
  ({
    sessionId,
    agentDid,
    requesterDid: null,
    behaviorId: null,
    title: sessionId,
    turnState: null,
    updatedAt: null,
  }) as SessionSummary;

/* two nodes; the route names the second */
const app = () =>
  testApp({
    deployments: [
      node({ agentDid: "did:key:here", sessions: [summary("did:key:here", "a")] }),
      node({ agentDid: "did:key:there", sessions: [summary("did:key:there", "b")] }),
    ],
  });

describe("a list opened on a node's route", () => {
  /* the route would narrow the list to its node again on the way back */
  it("lets go of the node in the route when the sessions filters are cleared", async () => {
    router.navigate.mockClear();
    renderIn(app(), <SessionsScreen nodeDid="did:key:there" />);
    await userEvent.click(screen.getByRole("button", { name: "Clear filters" }));
    expect(router.navigate).toHaveBeenCalledWith({ name: "sessions" });
  });

  it("lets go of the node in the route when the mailbox filters are cleared", async () => {
    router.navigate.mockClear();
    renderIn(app(), <MailboxScreen nodeDid="did:key:there" />);
    await userEvent.click(screen.getByRole("button", { name: "Clear filters" }));
    expect(router.navigate).toHaveBeenCalledWith({ name: "mailbox" });
  });
});
