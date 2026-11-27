import { describe, expect, it } from "vitest";

import type {
  LinkedSessionView,
  SessionProvenanceView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import {
  asksAgain,
  lineageSessions,
  sessionMoment,
  type LineageAsked,
} from "../../src/hooks/provenance";
import { loadLeanFamilyCases } from "./leanGenerator";

type Link = { node: string; session: string };
type Listed = { session: string; started_by: Link | null; moment: string };
type Input = {
  name: string;
  node: string;
  opened: string;
  lineage: {
    started_by: Link | null;
    started: Link[];
    sent: Link[];
    received: Link[];
    senders: Link[];
    calls: Link[];
    related: Link[];
  } | null;
  sessions: Listed[];
  asked: {
    cues: string;
    version: number | null;
    moments: { session: string; moment: string }[];
  } | null;
  cues: string;
  version: number | null;
};
type Decision = { movable: string[]; asks_again: boolean };

const leanFamilyCases = (path: string) =>
  loadLeanFamilyCases<{ input: Input; expected: Decision }>(path);

const linked = (link: Link): LinkedSessionView => ({
  nodeDid: link.node,
  sessionId: link.session,
  requesterDid: null,
  causeRequestDocId: "doc",
});

/** A listed session whose moment is the model's: its turn state, no update time. */
const summary = (node: string, listed: Listed) =>
  ({
    nodeDid: node,
    sessionId: listed.session,
    startedBy: listed.started_by ? linked(listed.started_by) : null,
    turnState: listed.moment,
    updatedAt: null,
  }) as unknown as SessionSummary;

const lineageOf = (lineage: NonNullable<Input["lineage"]>, opened: string) =>
  ({
    sessionId: opened,
    startedBy: lineage.started_by ? linked(lineage.started_by) : null,
    started: lineage.started.map(linked),
    sent: lineage.sent.map(linked),
    received: lineage.received.map(linked),
    senders: lineage.senders.map((link) => ({ requestId: "r", sender: linked(link) })),
    calls: lineage.calls.map((link) => ({
      requestId: "r",
      toolCallId: "c",
      caused: {
        requestId: "q",
        nodeDid: link.node,
        sessionId: link.session,
        requesterDid: null,
        lifecycleState: null,
        createdAt: null,
      },
    })),
    relatedSessions: lineage.related.map((link) =>
      summary(link.node, { session: link.session, started_by: null, moment: "" }),
    ),
  }) as SessionProvenanceView;

describe("generated lineage re-read", () => {
  it("drives lineageSessions and asksAgain with every Lean case", async () => {
    const cases = await leanFamilyCases(
      "families::lineage_reread_contracts_decision::cases()",
    );
    expect(cases.length).toBeGreaterThan(10);
    for (const { input, expected } of cases) {
      const sessions = input.sessions.map((listed) => summary(input.node, listed));
      const lineage = input.lineage ? lineageOf(input.lineage, input.opened) : null;
      const movable = lineageSessions(sessions, input.node, input.opened, lineage);
      expect(
        movable.map((s) => s.sessionId),
        input.name,
      ).toEqual(expected.movable);
      const asked: LineageAsked | null = input.asked && {
        cues: input.asked.cues,
        version: input.asked.version,
        listed: new Map(
          input.asked.moments.map(({ session, moment }) => [
            session,
            sessionMoment({
              turnState: moment,
              updatedAt: null,
            } as unknown as SessionSummary),
          ]),
        ),
      };
      expect(asksAgain(asked, input.cues, input.version, movable), input.name).toBe(
        expected.asks_again,
      );
    }
  }, 600_000);
});
