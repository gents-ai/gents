import { act, renderHook, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { resetSessionViews, seenMarks } from "../src/ui/app/sessionViews";
import { ListedSessionStatus } from "../src/ui/screens/SessionStatus";
import { sessionKeyOf } from "../src/hooks/fleetStore";
import { useSessionShown } from "../src/ui/hooks/useSessionShown";
import type { Route } from "../src/ui/lib/router";
import { node, publish, renderIn, testApp, withApp } from "./app-fixture";

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

  describe("on a phone, back at the sessions list", () => {
    const windowWide = (wide: boolean) =>
      vi.spyOn(window, "matchMedia").mockImplementation(
        (query) =>
          ({
            matches: query === "(min-width: 768px)" ? wide : false,
            media: query,
            addEventListener: () => {},
            removeEventListener: () => {},
          }) as unknown as MediaQueryList,
      );
    afterEach(() => vi.restoreAllMocks());

    const showing = (wide: boolean) => {
      windowWide(wide);
      const app = testApp({
        deployments: [node({ nodeDid: NODE, sessions: [session("r1", "completed")] })],
        selection: { nodeDid: NODE, sessionId: "s1" },
      });
      const page = renderHook((route: Route) => useSessionShown(route), {
        initialProps: { name: "session", sessionId: "s1", nodeDid: NODE } as Route,
        wrapper: withApp(app),
      });
      return { app, back: () => page.rerender({ name: "sessions" } as Route) };
    };
    const reply = (app: ReturnType<typeof testApp>) =>
      act(() =>
        publish(app, [node({ nodeDid: NODE, sessions: [session("r2", "completed")] })]),
      );
    const key = sessionKeyOf(session("r1", "completed"));

    it("marks a reply that lands after going back as new", () => {
      const { app, back } = showing(false);
      expect(seenMarks.of(key)).toEqual({ request: "r1", done: true });
      back();
      expect(app.stores.selection.getState().sessionId).toBe("s1");
      reply(app);
      expect(seenMarks.of(key)).toEqual({ request: "r1", done: true });
      renderIn(app, <ListedSessionStatus session={session("r2", "completed")} />);
      expect(
        screen.getByRole("img", { name: "Finished · new since you looked" }),
      ).toBeInTheDocument();
    });

    it("sees the reply once the session is open again", () => {
      const { app, back } = showing(false);
      back();
      reply(app);
      act(() => app.sessionShown.setState({ shown: true }));
      expect(seenMarks.of(key)).toEqual({ request: "r2", done: true });
    });

    it("keeps a wide window's selected session seen beside the list", () => {
      const { app, back } = showing(true);
      back();
      reply(app);
      expect(seenMarks.of(key)).toEqual({ request: "r2", done: true });
    });
  });
});
