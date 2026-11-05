import { act, fireEvent, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const desktop = (on: boolean) => {
  if (on) (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
  else delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
};

const session = (sessionId: string, updatedAt: string, nodeDid: string) => ({
  sessionId,
  nodeDid,
  title: sessionId,
  turnState: "completed",
  latestRequestId: `${sessionId}-req`,
  updatedAt,
});

/* after the reset, so the commands and the app share one module graph, with
   the desktop app's keys */
async function load() {
  vi.resetModules();
  const fixture = await import("./app-fixture");
  const { deployment } = await import("./config-panel-wiring/fixtures");
  const { Shortcuts } = await import("../src/ui/screens/Shortcuts");
  const { useFollowRoute } = await import("../src/ui/hooks/useFollowRoute");
  const { resetSessionTrail } = await import("../src/hooks/sessionTrail");
  const views = await import("../src/ui/app/sessionViews");
  const router = await import("../src/ui/lib/router");
  const commands = await import("../src/ui/commands/commands");
  const dispatch = await import("../src/ui/commands/dispatch");
  resetSessionTrail();
  views.resetSessionViews();
  views.seenMarks.start("2026-11-01T00:00:00Z");
  const node = deployment.nodeDid;
  const app = fixture.testApp({
    deployments: [
      fixture.node({
        sessions: [
          session("old", "2026-11-02T08:00:00Z", node),
          session("mid", "2026-11-02T09:00:00Z", node),
          session("new", "2026-11-02T10:00:00Z", node),
        ],
      }),
    ],
    selection: { nodeDid: node },
  });
  function Page({ route }: { route: Parameters<typeof useFollowRoute>[0] }) {
    useFollowRoute(route);
    return <Shortcuts />;
  }
  const view = fixture.renderRouted(app, { name: "sessions" }, (route) => (
    <Page route={route} />
  ));
  const open = () => {
    const route = view.nav().route as { name: string; sessionId?: string | null };
    return route.name === "session" ? route.sessionId : null;
  };
  const go = (sessionId: string) =>
    act(() => router.navigate({ name: "session", sessionId, nodeDid: node }));
  return { app, views, router, commands, dispatch, open, go };
}

/* jsdom is not a Mac: the command key is Ctrl */
const press = (key: string, shift = false) =>
  act(() => {
    fireEvent.keyDown(window, { key, ctrlKey: true, shiftKey: shift });
  });

describe("the session keys", () => {
  let unbind: (() => void) | null = null;
  beforeEach(() => desktop(true));
  afterEach(() => {
    unbind?.();
    unbind = null;
    desktop(false);
  });

  it("go back and forth between the two sessions open last", async () => {
    const { open, go } = await load();
    go("old");
    go("mid");
    expect(open()).toBe("mid");
    press(";");
    await waitFor(() => expect(open()).toBe("old"));
    press(";");
    await waitFor(() => expect(open()).toBe("mid"));
  }, 20_000);

  it("go to the session open last from a page that isn't one", async () => {
    const { open, go } = await load();
    go("old");
    go("mid");
    await act(async () => {
      const router = await import("../src/ui/lib/router");
      router.navigate({ name: "sessions" });
    });
    press(";");
    await waitFor(() => expect(open()).toBe("mid"));
  }, 20_000);

  it("ask first when going back would leave a draft behind", async () => {
    const { open, go, router } = await load();
    go("old");
    go("mid");
    const asked: unknown[] = [];
    unbind = router.bindNavigationGuard((route) => {
      asked.push(route);
      return false;
    });
    press(";");
    expect(asked).toHaveLength(1);
    expect(asked[0]).toMatchObject({ name: "session", sessionId: "old" });
    expect(open()).toBe("mid");
  }, 20_000);

  it("visit the sessions new since they were looked at, newest first", async () => {
    const { open, go, views } = await load();
    const { sessionKeyOf } = await import("../src/hooks/fleetStore");
    const { deployment } = await import("./config-panel-wiring/fixtures");
    /* "mid" was on screen since its work finished */
    views.seenMarks.set(
      sessionKeyOf({
        nodeDid: deployment.nodeDid,
        sessionId: "mid",
        requesterDid: null,
      }),
      { request: "mid-req", done: true },
    );
    press("A", true);
    await waitFor(() => expect(open()).toBe("new"));
    /* the one open is skipped, and the one seen isn't new */
    press("A", true);
    await waitFor(() => expect(open()).toBe("old"));
    go("old");
  }, 20_000);

  it("leave the key alone with no new session", async () => {
    const { views, commands, dispatch } = await load();
    const { sessionKeyOf } = await import("../src/hooks/fleetStore");
    const { deployment } = await import("./config-panel-wiring/fixtures");
    for (const id of ["old", "mid", "new"])
      views.seenMarks.set(
        sessionKeyOf({
          nodeDid: deployment.nodeDid,
          sessionId: id,
          requesterDid: null,
        }),
        { request: `${id}-req`, done: true },
      );
    const e = new KeyboardEvent("keydown", {
      key: "A",
      ctrlKey: true,
      shiftKey: true,
      cancelable: true,
    });
    expect(dispatch.dispatchKey(e, commands.listCommands(), false)).toBe(false);
    expect(e.defaultPrevented).toBe(false);
  }, 20_000);

  it("do nothing with ⌃; on a Mac, which isn't the command key there", async () => {
    const { go, commands, dispatch } = await load();
    go("old");
    go("mid");
    const e = new KeyboardEvent("keydown", {
      key: ";",
      ctrlKey: true,
      cancelable: true,
    });
    expect(dispatch.dispatchKey(e, commands.listCommands(), true)).toBe(false);
    expect(e.defaultPrevented).toBe(false);
  }, 20_000);
});
