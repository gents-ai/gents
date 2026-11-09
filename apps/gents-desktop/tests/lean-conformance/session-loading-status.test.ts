import { execFile } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { beforeAll, describe, expect, it, vi } from "vitest";

import type {
  DeploymentOperationalState,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
  OperationalStatus,
} from "@source-inc/gents-desktop-client";
import { BridgeInvokeError } from "@source-inc/gents-desktop-client";
import {
  UNREGISTERED_READ_MS,
  startClientObservation,
} from "../../src/hooks/clientObservation";
import { waitingRereadDelay } from "../../src/hooks/sessionReads";
import { readSession, writeSession } from "../../src/hooks/sessionStore";
import { node, testApp } from "../app-fixture";
import { sessionSnapshot } from "../timeline-fixture";
import {
  projectSessionLoadingStatus,
  selectionMatches,
  type SessionLoadState,
  type SessionLoadingStatus,
} from "../../src/lib/loadingStatus";

type Blocker = {
  layer: OperationalStatus["layer"];
  kind: OperationalStatus["kind"];
  label: string;
  detail: string;
  configureInference: boolean;
};

type StatusRow = {
  selected: boolean;
  sessionMatches: boolean;
  loadMatches: boolean;
  phase: SessionLoadState["phase"];
  error: string | null;
  found: boolean | null;
  reread: boolean;
  blocker: Blocker | null;
  expected: SessionLoadingStatus | null;
};

type Reply =
  | "found"
  | "missing"
  | "pending"
  | "delivering"
  | "notConnected"
  | "accepted"
  | "failed"
  | "mismatch";

type Act =
  | { kind: "select"; drop: boolean }
  | { kind: "notice" | "fire" | "retry" | "drop" | "stop" | "restart" }
  | { kind: "complete"; reply: Reply };

type Observation = {
  selected: number;
  status: Pick<SessionLoadingStatus, "layer" | "phase" | "title" | "action"> | null;
  loadPhase: SessionLoadState["phase"];
  load: number | null;
  reread: boolean | null;
  reading: boolean;
  shown: number | null;
};

type Cases = {
  statuses: StatusRow[];
  operational: StatusRow[];
  matches: {
    sameSession: boolean;
    selectedNode: string | null;
    targetNode: string | null;
    expected: boolean;
  }[];
  rereadBudget: number;
  rereads: { used: number; armed: boolean }[];
  traces: { acts: Act[]; obs: Observation[] }[];
};

const run = promisify(execFile);
const SELECTED = "session-selected";
const OTHER = "session-other";
const NODE = "did:key:node-0";

function operational(blocker: Blocker | null): DeploymentOperationalState | null {
  if (!blocker) return null;
  const admissionBlocker: OperationalStatus = {
    kind: blocker.kind,
    layer: blocker.layer,
    reason: "modeled",
    label: blocker.label,
    shortLabel: blocker.label,
    detail: blocker.detail,
    action: blocker.configureInference ? "configureInference" : null,
    animated: false,
  };
  return { admissionBlocker } as unknown as DeploymentOperationalState;
}

/* the modeled match facts become real ids, so the production match runs */
function project(row: StatusRow) {
  return projectSessionLoadingStatus({
    selectedSessionId: row.selected ? SELECTED : null,
    selectedNodeDid: NODE,
    session: { sessionId: row.sessionMatches ? SELECTED : OTHER, nodeDid: NODE },
    sessionLoad: {
      phase: row.phase,
      sessionId: row.loadMatches ? SELECTED : OTHER,
      nodeDid: NODE,
      found: row.found,
      error: row.error,
      reread: row.reread,
    },
    operationalState: operational(row.blocker),
  });
}

const sessionOf = (n: number) => `session-${n}`;
const numberOf = (id: string | null | undefined) =>
  id?.startsWith("session-") ? Number(id.slice("session-".length)) : null;

const RUNNING = {
  bootstrap: { clientStateExists: true, savedPeers: [] },
  client: {
    deployments: [
      node({
        nodeDid: NODE,
        sessions: [0, 1].map((n) => ({
          sessionId: sessionOf(n),
          nodeDid: NODE,
          requesterDid: null,
        })),
      }),
    ],
  },
} as unknown as DesktopClientSnapshot;
const STOPPED = { ...RUNNING, client: null } as unknown as DesktopClientSnapshot;

const WAITING_CODES: Partial<Record<Reply, string>> = {
  pending: "projectionPending",
  delivering: "sessionDelivering",
  notConnected: "sessionNotConnected",
  accepted: "sessionAccepted",
};

type Fetch = {
  sessionId: string;
  resolve: (snapshot: DesktopSessionSnapshot | null) => void;
  reject: (error: unknown) => void;
  settled: boolean;
};

/* The production observer, session reads and drain loop, driven through the
   bridge they read from: each session read waits for the trace to answer it,
   and registration and store events are the trace's notices. */
async function replay(acts: Act[]): Promise<Observation[]> {
  const fetches: Fetch[] = [];
  let client = RUNNING;
  const api = new Proxy(
    {
      fetchDesktopSnapshot: vi.fn(async () => client),
      fetchSessionSnapshot: vi.fn(
        (sessionId: string) =>
          new Promise<DesktopSessionSnapshot | null>((resolve, reject) => {
            const fetch: Fetch = {
              sessionId,
              settled: false,
              resolve: (value) => {
                fetch.settled = true;
                resolve(value);
              },
              reject: (error) => {
                fetch.settled = true;
                reject(error);
              },
            };
            fetches.push(fetch);
          }),
      ),
    } as Record<string, unknown>,
    {
      get: (target, key: string) =>
        target[key] ?? (target[key] = vi.fn(async () => null)),
    },
  );
  const app = testApp({
    api,
    snapshot: RUNNING,
    selection: { nodeDid: NODE, sessionId: sessionOf(0) },
  });
  let registration: (() => void) | null = null;
  let listener: ((event: { reason?: string }) => Promise<void>) | null = null;
  const stop = startClientObservation(app, async (handler) => {
    await new Promise<void>((resolve) => {
      registration = resolve;
    });
    registration = null;
    listener = handler as typeof listener;
    return () => {
      if (listener === handler) listener = null;
    };
  });
  const flush = () => vi.advanceTimersByTimeAsync(0);
  const observe = (): Observation => {
    const { sessionId, nodeDid } = app.stores.selection.getState();
    const { load } = app.stores.session.getState();
    const session = readSession(app.stores.session);
    const status = projectSessionLoadingStatus({
      selectedSessionId: sessionId,
      selectedNodeDid: nodeDid,
      session,
      sessionLoad: load,
      operationalState: null,
    });
    const loadMatches = load.sessionId === sessionId;
    const latest = fetches.at(-1);
    return {
      selected: numberOf(sessionId)!,
      status: status && {
        layer: status.layer,
        phase: status.phase,
        title: status.title,
        action: status.action,
      },
      loadPhase: load.phase,
      load: numberOf(load.sessionId),
      reread: loadMatches ? load.reread : null,
      reading: !!latest && !latest.settled,
      shown: numberOf(session?.sessionId),
    };
  };
  const observed: Observation[] = [];
  try {
    /* the observer's first read: the model owes it from the start, and the
       observer runs it by its deadline when the trace has not registered */
    await vi.advanceTimersByTimeAsync(UNREGISTERED_READ_MS);
    observed.push(observe());
    for (const act of acts) {
      switch (act.kind) {
        case "select": {
          if (act.drop) writeSession(app.stores.session, null);
          const next =
            numberOf(app.stores.selection.getState().sessionId) === 0 ? 1 : 0;
          app.stores.selection.setState({ sessionId: sessionOf(next) });
          break;
        }
        case "notice":
          if (registration) (registration as () => void)();
          /* the handler resolves once its read does, which the trace answers */
          else void listener?.({ reason: "store" });
          break;
        case "complete": {
          const fetch = fetches.at(-1)!;
          const code = WAITING_CODES[act.reply];
          if (code)
            fetch.reject(
              new BridgeInvokeError({
                code: code as never,
                message: `the runtime reports ${act.reply}`,
                retryable: true,
                endpoint: null,
              }),
            );
          else if (act.reply === "failed") fetch.reject(new Error("read failed"));
          else if (act.reply === "missing") fetch.resolve(null);
          else
            fetch.resolve(
              sessionSnapshot({
                sessionId: act.reply === "mismatch" ? "session-9" : fetch.sessionId,
                nodeDid: NODE,
              }),
            );
          break;
        }
        case "fire":
          await vi.advanceTimersToNextTimerAsync();
          break;
        case "retry": {
          /* what the status card's action runs */
          const selected = app.stores.selection.getState().sessionId;
          void Promise.all([
            app.actions.refreshSnapshot(),
            app.actions.refreshSession(selected),
          ]).catch(() => {});
          break;
        }
        case "drop":
          writeSession(app.stores.session, null);
          break;
        case "stop":
          client = STOPPED;
          app.stores.client.setState({ snapshot: STOPPED });
          /* the stopped observer's reads are answered and dropped */
          for (const fetch of fetches) if (!fetch.settled) fetch.resolve(null);
          break;
        case "restart":
          client = RUNNING;
          app.stores.client.setState({ snapshot: RUNNING });
          await vi.advanceTimersByTimeAsync(UNREGISTERED_READ_MS);
          break;
      }
      await flush();
      observed.push(observe());
    }
  } finally {
    stop();
  }
  return observed;
}

describe("generated selected-session load status", () => {
  let cases: Cases;
  beforeAll(async () => {
    const proofsDir = join(
      dirname(fileURLToPath(import.meta.url)),
      "../../../../crates/gents/proofs",
    );
    await run("lake", ["build", "Proofs.Conformance.ClientSessionLoad"], {
      cwd: proofsDir,
      timeout: 240_000,
    });
    const { stdout } = await run(
      "lake",
      ["env", "lean", "--run", "Proofs/Conformance/ClientSessionLoad.lean"],
      {
        cwd: proofsDir,
        encoding: "utf8",
        timeout: 240_000,
        maxBuffer: 64 * 1024 * 1024,
      },
    );
    cases = JSON.parse(stdout.trim()) as Cases;
  }, 300_000);

  it("drives the production projection, match and re-read budget with every Lean row", () => {
    expect(cases.statuses).toHaveLength(1536);
    expect(cases.operational).toHaveLength(80);
    for (const row of [...cases.statuses, ...cases.operational]) {
      expect(project(row), JSON.stringify(row)).toEqual(row.expected);
    }

    expect(cases.matches).toHaveLength(18);
    for (const row of cases.matches) {
      expect(
        selectionMatches(SELECTED, row.selectedNode, {
          sessionId: row.sameSession ? SELECTED : OTHER,
          nodeDid: row.targetNode,
        }),
      ).toBe(row.expected);
    }

    expect(cases.rereads).toHaveLength(cases.rereadBudget + 2);
    for (const row of cases.rereads) {
      expect(waitingRereadDelay(row.used) !== null).toBe(row.armed);
    }
  });

  it("replays every Lean follow-up trace through the observer, drain loop and session reads", async () => {
    expect(cases.traces.length).toBeGreaterThan(1_000);
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
    try {
      for (const trace of cases.traces) {
        expect(trace.obs).toHaveLength(trace.acts.length + 1);
        const observed = await replay(trace.acts);
        for (const [step, expected] of trace.obs.entries())
          expect(observed[step], JSON.stringify({ acts: trace.acts, step })).toEqual(
            expected,
          );
      }
    } finally {
      vi.useRealTimers();
    }
  }, 600_000);
});
