import { describe, expect, it } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { sessionKeyOf } from "../src/hooks/fleetStore";
import { sessionRows, type SessionRow } from "../src/ui/lib/session-rows";
import { stateOf } from "../src/ui/lib/session-filter";
import { turnLabel } from "../src/ui/lib/live";

const session = (sessionId: string, turnState: string | null = "completed") =>
  ({
    sessionId,
    nodeDid: "did:test:a",
    requesterDid: null,
    turnState,
  }) as SessionSummary;
const key = (s: SessionSummary) => sessionKeyOf(s);

/* lead started worker; worker started helper; stray's starter is not listed */
const lead = session("lead");
const worker = session("worker", "running");
const helper = session("helper", "failed");
const stray = session("stray");
const outside = session("outside");
const startedBy: Record<string, SessionSummary> = {
  worker: lead,
  helper: worker,
  stray: outside,
};
const parentOf = (s: SessionSummary) => startedBy[s.sessionId] ?? null;
const all = [lead, worker, helper, stray];

const shape = (rows: SessionRow[]) =>
  rows.map((row) =>
    row.kind === "toggle"
      ? `toggle ${row.kin}${row.open ? " open" : ""}`
      : `${row.child ? "  " : ""}${row.session.sessionId}${row.context ? " (context)" : ""}`,
  );

describe("the sessions list's rows", () => {
  it("keeps a session's started sessions behind a closed toggle, whether or not any still run", () => {
    const { rows, shown } = sessionRows({
      sessions: all,
      parentOf,
      matches: null,
      isOpen: () => false,
    });
    expect(shape(rows)).toEqual(["lead", "toggle 2", "stray"]);
    expect(shown).toBe(4);
    const settled = [lead, session("worker"), session("helper"), stray];
    expect(
      shape(
        sessionRows({ sessions: settled, parentOf, matches: null, isOpen: () => false })
          .rows,
      ),
    ).toEqual(["lead", "toggle 2", "stray"]);
  });

  it("opens what the person opened, a started session's own started sessions included", () => {
    const { rows } = sessionRows({
      sessions: all,
      parentOf,
      matches: null,
      isOpen: (k) => k === key(lead),
    });
    expect(shape(rows)).toEqual([
      "lead",
      "  worker",
      "  helper",
      "toggle 2 open",
      "stray",
    ]);
    expect(rows.find((r) => r.kind === "toggle")).toMatchObject({ running: 1 });
  });

  it("shows a session whose starter is out of the list as a row of its own", () => {
    const { rows } = sessionRows({
      sessions: [stray],
      parentOf,
      matches: null,
      isOpen: () => true,
    });
    expect(shape(rows)).toEqual(["stray"]);
  });

  it("while narrowed, shows a session for what it started, opens nothing and counts the matches", () => {
    const failed = (s: SessionSummary) => stateOf(s.turnState) === "failed";
    const closed = sessionRows({
      sessions: all,
      parentOf,
      matches: failed,
      isOpen: () => false,
    });
    expect(shape(closed.rows)).toEqual(["lead (context)", "  helper", "toggle 2"]);
    expect(closed.shown).toBe(1);
    const open = sessionRows({
      sessions: all,
      parentOf,
      matches: failed,
      isOpen: () => true,
    });
    expect(shape(open.rows)).toEqual([
      "lead (context)",
      "  worker (context)",
      "  helper",
      "toggle 2 open",
    ]);
  });

  it("shows each session caught in a loop of who started whom", () => {
    const a = session("a");
    const b = session("b");
    const loop = (s: SessionSummary) => (s.sessionId === "a" ? b : a);
    const { rows } = sessionRows({
      sessions: [a, b],
      parentOf: loop,
      matches: null,
      isOpen: () => false,
    });
    expect(rows.filter((r) => r.kind === "session")).toHaveLength(2);
  });
});

describe("a session's state", () => {
  it("names every turn state the summary carries", () => {
    expect(
      [
        "waitingForClaim",
        "running",
        "streaming",
        "failed",
        "error",
        "interrupted",
        "superseded",
        "cancelled",
        "completed",
        "idle",
        null,
      ].map(stateOf),
    ).toEqual([
      "waiting",
      "working",
      "working",
      "failed",
      "failed",
      "stopped",
      "stopped",
      "stopped",
      "finished",
      "finished",
      "finished",
    ]);
  });

  it("is said the way the filter names it", () => {
    expect(
      ["error", "dead", "FAILED", "cancelled", "superseded", "completed"].map((t) => [
        stateOf(t),
        turnLabel(t),
      ]),
    ).toEqual([
      ["failed", "Failed"],
      ["failed", "Failed"],
      ["failed", "Failed"],
      ["stopped", "Stopped"],
      ["stopped", "Superseded"],
      ["finished", null],
    ]);
  });
});
