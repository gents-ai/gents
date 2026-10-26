import { act, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { resetSessionViews } from "../src/ui/app/sessionViews";
import { ListedSessionStatus } from "../src/ui/screens/SessionStatus";
import { node, publish, renderIn, testApp } from "./app-fixture";

const NODE = "did:key:z6MkAgent";
const later = () => new Date(Date.now() + 60_000).toISOString();

const session = (latestRequestId: string, turnState: string): SessionSummary =>
  ({
    sessionId: "s1",
    nodeDid: NODE,
    requesterDid: null,
    startedBy: null,
    latestRequestId,
    turnState,
    updatedAt: later(),
    title: "Release check",
  }) as SessionSummary;

describe("the new mark", () => {
  beforeEach(resetSessionViews);

  it("shows when work finishes away from the session, and clears once it is on screen", () => {
    const app = testApp({
      deployments: [node({ nodeDid: NODE, sessions: [session("r1", "completed")] })],
    });
    act(() => app.stores.selection.setState({ sessionId: "s1" }));
    act(() => app.stores.selection.setState({ sessionId: null }));
    act(() =>
      publish(app, [node({ nodeDid: NODE, sessions: [session("r2", "completed")] })]),
    );
    const { rerender } = renderIn(
      app,
      <ListedSessionStatus session={session("r2", "completed")} />,
    );
    expect(
      screen.getByRole("img", { name: "Finished · new since you looked" }),
    ).toBeInTheDocument();

    act(() => app.stores.selection.setState({ sessionId: "s1" }));
    rerender(<ListedSessionStatus session={session("r2", "completed")} />);
    expect(
      screen.queryByRole("img", { name: "Finished · new since you looked" }),
    ).toBeNull();
  });
});
