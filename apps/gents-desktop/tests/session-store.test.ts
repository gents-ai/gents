import { describe, expect, it } from "vitest";

import type {
  DesktopSessionSnapshot,
  RenderedTimelineItem,
} from "@source-inc/gents-desktop-client";
import { createSessionStore, writeSession } from "../src/hooks/sessionStore";

const user = (requestId: string) =>
  ({
    kind: "userMessage",
    itemKey: `u-${requestId}`,
    requestId,
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
});
