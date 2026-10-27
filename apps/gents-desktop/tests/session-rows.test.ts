import { describe, expect, it } from "vitest";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { sessionKeyOf } from "../src/hooks/fleetStore";
import { OPENED_SHOWN, sessionRows, type SessionRow } from "../src/ui/lib/session-rows";
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
  rows.map(
    (row) =>
      `${"  ".repeat(row.depth)}${row.session.sessionId}` +
      (row.started ? ` [${row.started}${row.open ? " open" : ""}]` : "") +
      (row.path ? ` in ${row.path.map((s) => s.sessionId).join("/") || "."}` : ""),
  );
const opened =
  (...open: SessionSummary[]) =>
  (k: string) =>
    open.some((s) => key(s) === k);

describe("the sessions list's rows", () => {
  it("puts pinned sessions first among the sessions beside them, in list order", () => {
    const second = session("second");
    const sideline = session("sideline");
    const { rows } = sessionRows({
      sessions: [lead, worker, sideline, stray, second],
      parentOf: (s) => (s === sideline ? lead : parentOf(s)),
      under: null,
      matches: null,
      isOpen: opened(lead),
      pinned: new Set([key(second), key(stray), key(sideline)]),
    });
    expect(shape(rows)).toEqual([
      "stray",
      "second",
      "lead [2 open]",
      "  sideline",
      "  worker",
    ]);
    expect(
      rows.filter((row) => row.pinned).map((row) => row.session.sessionId),
    ).toEqual(["stray", "second", "sideline"]);
  });

  it("keeps every session's started sessions closed until opened, whether or not any still run", () => {
    const { rows, shown, total } = sessionRows({
      sessions: all,
      parentOf,
      under: null,
      matches: null,
      isOpen: () => false,
    });
    expect(shape(rows)).toEqual(["lead [1]", "stray"]);
    expect([shown, total]).toEqual([2, 4]);
    const settled = [lead, session("worker"), session("helper"), stray];
    expect(
      shape(
        sessionRows({
          sessions: settled,
          parentOf,
          under: null,
          matches: null,
          isOpen: () => false,
        }).rows,
      ),
    ).toEqual(["lead [1]", "stray"]);
  });

  it("opens one level at a time, each started session one step under its starter", () => {
    const lead1 = sessionRows({
      sessions: all,
      parentOf,
      under: null,
      matches: null,
      isOpen: opened(lead),
    });
    expect(shape(lead1.rows)).toEqual(["lead [1 open]", "  worker [1]", "stray"]);
    const both = sessionRows({
      sessions: all,
      parentOf,
      under: null,
      matches: null,
      isOpen: opened(lead, worker),
    });
    expect(shape(both.rows)).toEqual([
      "lead [1 open]",
      "  worker [1 open]",
      "    helper",
      "stray",
    ]);
    expect(both.rows[0]).toMatchObject({ running: 1 });
    expect(both.rows[1]).toMatchObject({ running: 0 });
  });

  it("hides everything under a closed session and keeps what is open below it", () => {
    const { rows } = sessionRows({
      sessions: all,
      parentOf,
      under: null,
      matches: null,
      isOpen: opened(worker),
    });
    expect(shape(rows)).toEqual(["lead [1]", "stray"]);
  });

  it("shows a session whose starter is out of the list as a row of its own", () => {
    const { rows } = sessionRows({
      sessions: [stray],
      parentOf,
      under: null,
      matches: null,
      isOpen: () => true,
    });
    expect(shape(rows)).toEqual(["stray"]);
  });

  it("while narrowed, lists every match at or below the level, flat, with the path to each", () => {
    const failed = (s: SessionSummary) => stateOf(s.turnState) === "failed";
    for (const isOpen of [() => false, () => true]) {
      const top = sessionRows({
        sessions: all,
        parentOf,
        under: null,
        matches: failed,
        isOpen,
      });
      expect(shape(top.rows)).toEqual(["helper in lead/worker"]);
      expect([top.shown, top.total]).toEqual([1, 4]);
    }
    const below = sessionRows({
      sessions: all,
      parentOf,
      under: key(lead),
      matches: (s) => s.sessionId !== "stray",
    });
    expect(shape(below.rows)).toEqual(["worker [1] in .", "helper in worker"]);
    expect([below.shown, below.total]).toEqual([2, 2]);
  });

  it("shows one level: what a session started, with the trail down to it", () => {
    const { rows, trail, shown } = sessionRows({
      sessions: all,
      parentOf,
      under: key(worker),
      matches: null,
    });
    expect(shape(rows)).toEqual(["helper"]);
    expect(trail.map((s) => s.sessionId)).toEqual(["lead", "worker"]);
    expect(shown).toBe(1);
  });

  it("shows the top level when the session it was under is no longer listed", () => {
    const { rows, trail } = sessionRows({
      sessions: all,
      parentOf,
      under: key(outside),
      matches: null,
    });
    expect(shape(rows)).toEqual(["lead [1]", "stray"]);
    expect(trail).toEqual([]);
  });

  it("names a session's starter and the level that starter is shown on", () => {
    const { rows } = sessionRows({
      sessions: all,
      parentOf,
      under: null,
      matches: null,
      isOpen: () => true,
    });
    const starterOf = (id: string) =>
      rows.find((r) => r.session.sessionId === id)!.starter;
    expect(starterOf("lead")).toBeNull();
    expect(starterOf("worker")).toEqual({ key: key(lead), level: null });
    expect(starterOf("helper")).toEqual({ key: key(worker), level: key(lead) });
  });

  it("opens a group to its first few and anything still working, counting the rest", () => {
    const many = Array.from({ length: OPENED_SHOWN + 3 }, (_, n) =>
      session(`w${n}`, n === OPENED_SHOWN + 1 ? "running" : "completed"),
    );
    const { rows } = sessionRows({
      sessions: [lead, ...many],
      parentOf: (s) => (s === lead ? null : lead),
      under: null,
      matches: null,
      isOpen: () => true,
    });
    expect(shape(rows)).toEqual([
      `lead [${many.length} open]`,
      ...many.slice(0, OPENED_SHOWN).map((s) => `  ${s.sessionId}`),
      `  w${OPENED_SHOWN + 1}`,
    ]);
    expect(rows[0]).toMatchObject({ hidden: 2, running: 1 });
    const level = sessionRows({
      sessions: [lead, ...many],
      parentOf: (s) => (s === lead ? null : lead),
      under: key(lead),
      matches: null,
    });
    expect(level.rows).toHaveLength(many.length);
  });

  it("puts each started session right after its own starter, whatever the list order", () => {
    const second = session("second");
    const deep = session("deep");
    const by: Record<string, SessionSummary> = {
      worker: lead,
      second: lead,
      deep: worker,
    };
    const { rows } = sessionRows({
      sessions: [deep, second, lead, worker],
      parentOf: (s) => by[s.sessionId] ?? null,
      under: null,
      matches: null,
      isOpen: () => true,
    });
    expect(shape(rows)).toEqual([
      "lead [2 open]",
      "  second",
      "  worker [1 open]",
      "    deep",
    ]);
  });

  it("counts depth by the sessions listed, through starters the list leaves out", () => {
    const listedParent = (s: SessionSummary) =>
      s.sessionId === "worker" ? lead : null;
    const { rows } = sessionRows({
      sessions: [lead, worker],
      parentOf: listedParent,
      under: null,
      matches: null,
      isOpen: () => true,
    });
    expect(shape(rows)).toEqual(["lead [1 open]", "  worker"]);
  });

  it("shows each session caught in a loop of who started whom", () => {
    const a = session("a");
    const b = session("b");
    const loop = (s: SessionSummary) => (s.sessionId === "a" ? b : a);
    const { rows } = sessionRows({
      sessions: [a, b],
      parentOf: loop,
      under: null,
      matches: null,
      isOpen: () => false,
    });
    expect(rows).toHaveLength(2);
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
