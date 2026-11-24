import { act, cleanup, fireEvent } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { SessionsScreen } from "../src/ui/screens/SessionsScreen";
import { resetListViews } from "../src/ui/app/listViews";
import { navigate, type Route } from "../src/ui/lib/router";
import { node, renderRouted, testApp } from "./app-fixture";

const summary = (nodeDid: string, sessionId: string) =>
  ({
    sessionId,
    nodeDid,
    requesterDid: null,
    archivedAt: null,
    agentDid: null,
    title: sessionId,
    turnState: null,
    updatedAt: null,
  }) as SessionSummary;
const sessionsOn = (nodeDid: string) =>
  node({
    nodeDid,
    sessions: Array.from({ length: 40 }, (_, n) => summary(nodeDid, `${nodeDid}-${n}`)),
  });
const page = (route: Route) =>
  route.name === "sessions" ? <SessionsScreen entry={route} /> : <p>a session</p>;

beforeEach(() => {
  vi.stubGlobal("localStorage", {
    getItem: () => null,
    setItem: vi.fn(),
    removeItem: vi.fn(),
  });
  resetListViews();
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("the sessions list's place", () => {
  it("is where it was left after a session is opened and closed, for each node on its own", () => {
    const app = testApp({
      deployments: [sessionsOn("did:key:a"), sessionsOn("did:key:b")],
    });
    act(() => app.actions.selectNode("did:key:a"));
    const view = renderRouted(app, { name: "sessions" }, page);
    const viewport = () =>
      view.container.querySelector<HTMLElement>("[data-slot=scroll-area-viewport]");

    viewport()!.scrollTop = 480;
    fireEvent.scroll(viewport()!);
    act(() => navigate({ name: "session", sessionId: "did:key:a-20" }));
    expect(viewport()).toBeNull();
    act(() => view.nav().back());
    expect(viewport()!.scrollTop).toBe(480);

    act(() => app.actions.selectNode("did:key:b"));
    expect(viewport()!.scrollTop).toBe(0);
    viewport()!.scrollTop = 120;
    fireEvent.scroll(viewport()!);
    act(() => app.actions.selectNode("did:key:a"));
    expect(viewport()!.scrollTop).toBe(480);
    act(() => app.actions.selectNode("did:key:b"));
    expect(viewport()!.scrollTop).toBe(120);
  });
});
