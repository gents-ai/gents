import { act, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { resetSessionViews, seenMarks } from "../src/ui/app/sessionViews";
import { ListedSessionStatus } from "../src/ui/screens/SessionStatus";
import { sessionKeyOf } from "../src/hooks/fleetStore";
import { node, publish, renderIn, testApp } from "./app-fixture";

const NODE = "did:key:z6MkAgent";
const OTHER_NODE = "did:key:z6MkOther";
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

  it("keeps seen marks separate when two nodes use the same session ID", () => {
    const onFirst = session("first-request", "completed");
    const onSecond = { ...session("second-request", "completed"), nodeDid: OTHER_NODE };
    const app = testApp({
      deployments: [
        node({ nodeDid: NODE, sessions: [onFirst] }),
        node({ nodeDid: OTHER_NODE, sessions: [onSecond] }),
      ],
      selection: { nodeDid: OTHER_NODE, sessionId: "s1" },
    });

    expect(seenMarks.of(sessionKeyOf(onFirst))).toBeNull();
    expect(seenMarks.of(sessionKeyOf(onSecond))).toEqual({
      request: "second-request",
      done: true,
    });
    act(() => app.stores.selection.setState({ nodeDid: NODE, sessionId: "s1" }));
    expect(seenMarks.of(sessionKeyOf(onFirst))).toEqual({
      request: "first-request",
      done: true,
    });

    const { rerender } = renderIn(
      app,
      <ListedSessionStatus session={{ ...onSecond, latestRequestId: "new-request" }} />,
    );
    expect(
      screen.getByRole("img", { name: "Finished · new since you looked" }),
    ).toBeInTheDocument();

    rerender(<ListedSessionStatus session={onFirst} />);
    expect(
      screen.queryByRole("img", { name: "Finished · new since you looked" }),
    ).toBeNull();
  });
});
