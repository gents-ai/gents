import { describe, expect, it } from "vitest";

import type {
  DesktopSessionSnapshot,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";
import {
  createSessionStore,
  holdsRequest,
  writeSession,
} from "../src/hooks/sessionStore";

const user = (requestId: string) =>
  ({
    kind: "userMessage",
    itemKey: `u-${requestId}`,
    requestId,
    ownsTurn: true,
  }) as RenderedTimelineItem;
/* a row the request authors beside its prompt: saved, but not the turn */
const context = (requestId: string) =>
  ({
    kind: "userMessage",
    itemKey: `authored:doc-${requestId}:context`,
    requestId,
    ownsTurn: false,
  }) as RenderedTimelineItem;
const tools = (key: string, status: string) =>
  ({
    kind: "toolGroup",
    itemKey: key,
    tools: [{ itemKey: `${key}-t`, statusKind: status }],
  }) as unknown as RenderedTimelineItem;
const live = (content: string) =>
  ({ kind: "liveAssistant", itemKey: "live", content }) as RenderedTimelineItem;
const session = (timelineItems: RenderedTimelineItem[]) =>
  ({ sessionId: "s", timelineItems }) as unknown as DesktopSessionSnapshot;

/* the facts screens read besides drawing the transcript, kept on write */
describe("session facts", () => {
  it("leave a streamed chunk to the live reply uncounted", () => {
    const row = user("r1");
    const group = tools("g1", "running");
    const store = createSessionStore(session([row, group, live("a")]));
    const before = store.getState().facts;
    writeSession(store, session([row, group, live("ab")]));
    expect(store.getState().facts).toBe(before);
  });

  it("count a new row, and a changed tool group as a tool change", () => {
    const row = user("r1");
    const store = createSessionStore(session([row, tools("g1", "running")]));
    const first = store.getState().facts;
    expect(first.userRequestIds).toEqual(new Set(["r1"]));
    expect(first.tools).toHaveLength(1);

    writeSession(store, session([row, tools("g1", "success"), user("r2")]));
    const second = store.getState().facts;
    expect(second.rowsRevision).toBe(first.rowsRevision + 1);
    expect(second.toolsRevision).toBe(first.toolsRevision + 1);
    expect(second.userRequestIds).toEqual(new Set(["r1", "r2"]));
  });

  it("keep the tool list while only messages change", () => {
    const group = tools("g1", "success");
    const store = createSessionStore(session([user("r1"), group]));
    const first = store.getState().facts;
    writeSession(store, session([user("r1"), group, user("r2")]));
    const second = store.getState().facts;
    expect(second.rowsRevision).toBe(first.rowsRevision + 1);
    expect(second.toolsRevision).toBe(first.toolsRevision);
    expect(second.tools).toBe(first.tools);
  });

  it("leave a request's context row out: saved, but not a turn", () => {
    const store = createSessionStore(session([user("r1"), context("r2")]));
    expect(store.getState().facts.userRequestIds).toEqual(new Set(["r1"]));
  });
});

/* A request the app sent, known by its id on every row the bridge draws
   for it; the rows that stand for its turn — its pending turn, its saved
   prompt — are the ones that hold it. */
describe("whether the bridge holds a sent request", () => {
  const pending = (requestId: string) =>
    ({
      kind: "pendingUserTurn",
      itemKey: `pending-${requestId}`,
      requestId,
    }) as RenderedTimelineItem;
  const read = (latestRequestId: string | null, items: RenderedTimelineItem[]) =>
    createSessionStore({
      sessionId: "s",
      latestRequestId,
      timelineItems: items,
    } as unknown as DesktopSessionSnapshot).getState();
  const sent = { sessionId: "s", requestId: "r2", latestWhenSent: "r1" };

  it("not before any read shows it", () => {
    expect(holdsRequest(read("r1", [user("r1")]), sent)).toBe(false);
  });

  it("once its pending turn is in the transcript", () => {
    expect(holdsRequest(read("r1", [pending("r2")]), sent)).toBe(true);
  });

  it("once its saved message is in the transcript", () => {
    expect(holdsRequest(read("r1", [user("r2")]), sent)).toBe(true);
  });

  it("not while only the context row it authors is in the transcript", () => {
    expect(holdsRequest(read("r1", [context("r2")]), sent)).toBe(false);
  });

  it("once it is the session's latest request, its row outside the window", () => {
    expect(holdsRequest(read("r2", [user("r1")]), sent)).toBe(true);
  });

  it("once the latest request has moved past the one there when it was sent", () => {
    expect(holdsRequest(read("r3", [user("r1")]), sent)).toBe(true);
  });

  it("never in another session", () => {
    expect(holdsRequest(read("r2", [pending("r2")]), { ...sent, sessionId: "t" })).toBe(
      false,
    );
  });
});
