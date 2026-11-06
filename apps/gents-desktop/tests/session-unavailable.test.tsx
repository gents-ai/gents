import { act, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup } from "@testing-library/react";
import { BridgeInvokeError } from "@source-inc/gents-desktop-client";
import { OPEN_ON_WAIT_MS } from "../src/hooks/selectionReconcile";
import { writeSessionLoad } from "../src/hooks/sessionStore";
import { chat } from "../src/hooks/chatStore";
import {
  SessionUnavailable,
  missingSessionText,
  sessionReport,
  useSessionAbsence,
} from "../src/ui/screens/session/SessionUnavailable";
import { useFollowRoute } from "../src/ui/hooks/useFollowRoute";
import { SessionScreen } from "../src/ui/screens/SessionScreen";
import { node, publish, renderIn, testApp } from "./app-fixture";

const NODE = node().nodeDid;
const ELSEWHERE = "did:key:z6MkElsewhere";

function Probe() {
  const absence = useSessionAbsence(true);
  if (absence === "waiting") return <p>still loading</p>;
  return absence === "missing" ? <SessionUnavailable /> : <p>the session screen</p>;
}

const appOn = (
  nodeDid: string,
  deployments: unknown[] = [node({ label: "Forge" })],
  api = {},
) => {
  const app = testApp({
    api: { fetchSessionSnapshot: vi.fn().mockResolvedValue(null), ...api },
    deployments,
    selection: { nodeDid, sessionId: "s-gone" },
  });
  return app;
};

const lookedAndFoundNothing = (app: ReturnType<typeof testApp>, sessionId = "s-gone") =>
  act(() =>
    writeSessionLoad(app.stores.session, {
      phase: "loaded",
      sessionId,
      nodeDid: NODE,
      found: false,
      error: null,
    }),
  );

describe("a session link that leads nowhere", () => {
  afterEach(cleanup);

  it("says the session wasn't found once it has been looked for, and offers a way on", () => {
    const app = appOn(NODE);
    renderIn(app, <Probe />);
    expect(screen.getByText("the session screen")).toBeTruthy();
    lookedAndFoundNothing(app);
    expect(screen.getByRole("heading", { name: "Session not found" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Try again" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Sessions" })).toBeTruthy();
  });

  it("takes the node's own answer that it has no record of the session", async () => {
    const fetchSessionSnapshot = vi.fn().mockRejectedValue(
      new BridgeInvokeError({
        code: "notFound",
        message:
          "Forge has no record of this session, and this computer sent nothing to it.",
        retryable: false,
        endpoint: NODE,
      }),
    );
    const app = appOn(NODE, undefined, { fetchSessionSnapshot });
    renderIn(app, <Probe />);
    await act(async () => void (await app.actions.refreshSession("s-gone")));
    expect(screen.getByRole("heading", { name: "Session not found" })).toBeTruthy();
    /* looking again holds the page rather than flashing back to loading */
    fetchSessionSnapshot.mockReturnValue(new Promise(() => {}));
    act(() => void app.actions.refreshSession("s-gone"));
    expect(screen.getByRole("heading", { name: "Session not found" })).toBeTruthy();
  });

  it("keeps other failures as a failure to load, with their retry", async () => {
    const fetchSessionSnapshot = vi.fn().mockRejectedValue(
      new BridgeInvokeError({
        code: "unknown",
        message: "the read failed",
        retryable: true,
        endpoint: null,
      }),
    );
    const app = appOn(NODE, undefined, { fetchSessionSnapshot });
    renderIn(app, <Probe />);
    await act(async () => void (await app.actions.refreshSession("s-gone")));
    expect(screen.getByText("the session screen")).toBeTruthy();
    expect(app.stores.session.getState().load.phase).toBe("failed");
  });

  it("doesn't claim whether it was removed or isn't shared, which a node can't say yet", () => {
    const { detail } = missingSessionText(
      testApp({ deployments: [node({ label: "Forge" })] }).stores.fleet.getState(),
      NODE,
    );
    expect(detail).toMatch(/removed/);
    expect(detail).toMatch(/shared with you/);
  });

  it("stays on the active node when a link names one this app doesn't know", () => {
    const app = testApp({
      api: { fetchSessionSnapshot: vi.fn().mockResolvedValue(null) },
      deployments: [node({ label: "Forge" })],
      selection: { nodeDid: NODE, sessionId: null },
    });
    function Page() {
      useFollowRoute({ name: "session", sessionId: "s-gone", nodeDid: ELSEWHERE });
      return <SessionScreen nodeDid={ELSEWHERE} sessionId="s-gone" />;
    }
    renderIn(app, <Page />);
    expect(
      screen.getByRole("heading", {
        name: "This link names a node you aren't connected to",
      }),
    ).toBeTruthy();
    expect(app.stores.selection.getState()).toMatchObject({
      nodeDid: NODE,
      sessionId: null,
    });
    /* a snapshot arriving later doesn't drag the app across either */
    act(() => publish(app, [node({ label: "Forge" })]));
    expect(app.stores.selection.getState().nodeDid).toBe(NODE);
  });

  it("opens the session once the node it names is listed", () => {
    const app = testApp({
      api: { fetchSessionSnapshot: vi.fn().mockResolvedValue(null) },
      deployments: [node({ label: "Forge" })],
      selection: { nodeDid: NODE, sessionId: null },
    });
    function Page() {
      useFollowRoute({ name: "session", sessionId: "s-gone", nodeDid: ELSEWHERE });
      return null;
    }
    renderIn(app, <Page />);
    act(() =>
      publish(app, [
        node({ label: "Forge" }),
        node({ nodeDid: ELSEWHERE, label: "Juno" }),
      ]),
    );
    expect(app.stores.selection.getState()).toMatchObject({
      nodeDid: ELSEWHERE,
      sessionId: "s-gone",
    });
  });

  it("says when its node can't be reached", () => {
    const app = appOn(NODE, [
      node({ label: "Forge", dialSucceeded: false, lastError: "connection refused" }),
    ]);
    renderIn(app, <Probe />);
    lookedAndFoundNothing(app);
    expect(screen.getByRole("heading", { name: /^Can't reach/ })).toBeTruthy();
  });

  it("keeps loading while its node connects, and reads a connection as it arrives", () => {
    const app = appOn(NODE, [
      node({ label: "Forge", dialSucceeded: false, lastError: null }),
    ]);
    renderIn(app, <Probe />);
    lookedAndFoundNothing(app);
    expect(screen.getByText("still loading")).toBeTruthy();
    act(() => publish(app, [node({ label: "Forge", dialSucceeded: true })]));
    expect(screen.getByRole("heading", { name: "Session not found" })).toBeTruthy();
  });

  it("stops waiting for a node that dropped, which reports no error", () => {
    vi.useFakeTimers();
    try {
      const app = appOn(NODE, [
        node({ label: "Forge", dialSucceeded: false, lastError: null }),
      ]);
      renderIn(app, <Probe />);
      lookedAndFoundNothing(app);
      expect(screen.getByText("still loading")).toBeTruthy();
      act(() => void vi.advanceTimersByTime(OPEN_ON_WAIT_MS));
      expect(screen.getByRole("heading", { name: /^Can't reach/ })).toBeTruthy();
    } finally {
      vi.useRealTimers();
    }
  });

  it("names no node it doesn't know, and doesn't say a session is on one", () => {
    const fleet = testApp({
      deployments: [node({ label: "Forge" })],
    }).stores.fleet.getState();
    expect(missingSessionText(fleet, "did:key:z6MkElsewhere").detail).toMatch(
      /on your nodes/,
    );
  });

  it("keeps what to pass on behind Details, without the error's type", async () => {
    const fetchSessionSnapshot = vi.fn().mockRejectedValue(
      new BridgeInvokeError({
        code: "notFound",
        message: "Forge has no record of this session.",
        retryable: false,
        endpoint: NODE,
      }),
    );
    const app = appOn(NODE, undefined, { fetchSessionSnapshot });
    renderIn(app, <Probe />);
    await act(async () => void (await app.actions.refreshSession("s-gone")));
    const details = screen.getByRole("button", { name: "Details" });
    expect(details.getAttribute("aria-expanded")).toBe("false");
    const report = document.getElementById(details.getAttribute("aria-controls")!)!;
    expect(report.hidden).toBe(true);
    act(() => details.click());
    expect(report.hidden).toBe(false);
    expect(report.textContent).toContain("Session: s-gone");
    expect(report.textContent).toContain(`(${NODE})`);
    expect(report.textContent).toContain(
      "Answer: Forge has no record of this session.",
    );
    expect(report.textContent).not.toContain("BridgeInvokeError");
  });

  it("names a node off the list as such in Details", () => {
    const fleet = testApp({
      deployments: [node({ label: "Forge" })],
    }).stores.fleet.getState();
    expect(sessionReport("s", fleet, ELSEWHERE, null)).toBe(
      `Session: s\nNode: ${ELSEWHERE} (not on this app's list)`,
    );
  });

  it("looks again on Try again", async () => {
    const fetchSessionSnapshot = vi.fn().mockResolvedValue(null);
    const app = appOn(NODE, undefined, {
      fetchSessionSnapshot,
      fetchSnapshot: vi.fn(),
    });
    const refresh = vi
      .spyOn(app.actions, "refreshSession")
      .mockResolvedValue(null as never);
    vi.spyOn(app.actions, "refreshSnapshot").mockResolvedValue(undefined as never);
    renderIn(app, <Probe />);
    lookedAndFoundNothing(app);
    act(() => screen.getByRole("button", { name: "Try again" }).click());
    await waitFor(() => expect(refresh).toHaveBeenCalledWith("s-gone"));
  });

  it("never covers a session this app just sent into, before any node has it", () => {
    const app = appOn(NODE);
    act(() =>
      chat.showPendingTurn(app.stores.chat, {
        sessionId: "s-gone",
        requestId: "r-1",
      } as never),
    );
    act(() => chat.endPendingTurn(app.stores.chat, "r-1"));
    renderIn(app, <Probe />);
    lookedAndFoundNothing(app);
    expect(screen.getByText("the session screen")).toBeTruthy();
  });

  it("waits while a node still lists the session", () => {
    const app = appOn(NODE, [
      node({ label: "Forge", sessions: [{ sessionId: "s-gone", nodeDid: NODE }] }),
    ]);
    renderIn(app, <Probe />);
    lookedAndFoundNothing(app);
    expect(screen.getByText("the session screen")).toBeTruthy();
  });

  it("isn't fooled by a look that was for another session", () => {
    const app = appOn(NODE);
    renderIn(app, <Probe />);
    lookedAndFoundNothing(app, "s-other");
    expect(screen.getByText("the session screen")).toBeTruthy();
  });
});
