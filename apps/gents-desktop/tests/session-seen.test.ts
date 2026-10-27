import { describe, expect, it } from "vitest";
import { isNew, seenMarkOf } from "../src/ui/lib/seen";

const SINCE = "2026-10-26T12:00:00Z";
const done = (latestRequestId: string, over = {}) => ({
  latestRequestId,
  turnState: "completed",
  updatedAt: "2026-10-26T13:00:00Z",
  startedBy: null,
  ...over,
});

describe("new since you looked", () => {
  it("is new when finished work arrived after the session was last on screen", () => {
    expect(isNew(done("r2"), { request: "r1", done: true }, SINCE)).toBe(true);
    expect(isNew(done("r1"), { request: "r1", done: false }, SINCE)).toBe(true);
  });

  it("is not new when nothing finished since, whatever moved its activity time", () => {
    expect(isNew(done("r1"), { request: "r1", done: true }, SINCE)).toBe(false);
    expect(
      isNew(done("r2", { turnState: "running" }), { request: "r1", done: true }, SINCE),
    ).toBe(false);
  });

  it("counts a session never seen here only for work after the device started keeping marks", () => {
    expect(isNew(done("r1"), null, SINCE)).toBe(true);
    expect(
      isNew(done("r1", { updatedAt: "2026-10-26T11:00:00+00:00" }), null, SINCE),
    ).toBe(false);
    expect(isNew(done("r1"), null, null)).toBe(false);
  });

  it("leaves a started session to its starter", () => {
    const started = {
      sessionId: "lead",
      nodeDid: "n",
      requesterDid: null,
      causeRequestDocId: "d",
    };
    expect(
      isNew(done("r2", { startedBy: started }), { request: "r1", done: true }, SINCE),
    ).toBe(false);
  });

  it("keeps the latest request and whether it had finished as the mark", () => {
    expect(seenMarkOf(done("r1"))).toEqual({ request: "r1", done: true });
    expect(seenMarkOf(done("r1", { turnState: "running" }))).toEqual({
      request: "r1",
      done: false,
    });
  });
});
