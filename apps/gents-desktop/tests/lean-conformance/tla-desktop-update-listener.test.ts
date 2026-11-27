import { expect, it, vi } from "vitest";
import {
  BridgeInvokeError,
  type DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";
import { startClientObservation } from "../../src/hooks/clientObservation";
import { BURST_MS } from "../../src/hooks/projectionController";
import { readSession } from "../../src/hooks/sessionStore";
import { projectSessionLoadingStatus } from "../../src/lib/loadingStatus";
import { node, testApp } from "../app-fixture";
import { deferred } from "../deferred";
import { sessionSnapshot, userMessage } from "../timeline-fixture";
import failure from "../../../../crates/gents/proofs/tla/generated/traces/MCDesktopUpdateListenerListenFailure.json";
import startup from "../../../../crates/gents/proofs/tla/generated/traces/MCDesktopUpdateListenerNoInitialRead.json";
import silent from "../../../../crates/gents/proofs/tla/generated/traces/MCDesktopUpdateListenerPendingNoEvent.json";
import gap from "../../../../crates/gents/proofs/tla/generated/traces/MCDesktopUpdateListenerRegistrationGap.json";
import mismatch from "../../../../crates/gents/proofs/tla/generated/traces/MCDesktopUpdateListenerSupersededMismatch.json";

import convergence from "../../../../crates/gents/proofs/tla/generated/traces/MCDesktopUpdateListener.sample-1.json";
import relisten from "../../../../crates/gents/proofs/tla/generated/traces/MCDesktopUpdateListener.sample-2.json";

it.each([failure, startup, silent, gap, mismatch, convergence, relisten])(
  "replays $config through the desktop observer",
  async (trace) => {
    expect(trace.spec).toBe("DesktopUpdateListener");
    const nodeDid = "did:key:listener-node";
    const snapshot = {
      bootstrap: { clientStateExists: true, savedRuntimes: [] },
      client: {
        deployments: [
          node({
            nodeDid,
            sessions: [1, 2].map((id) => ({
              sessionId: `s-${id}`,
              nodeDid,
              requesterDid: null,
            })),
          }),
        ],
      },
    };
    let version = 0;
    let arrived = false;
    let handler: ((event: { reason?: string }) => Promise<void>) | undefined;
    let registration = deferred();
    let registrations = 0;
    let notice = false;
    type Read = {
      sessionId: string;
      version: number;
      response: ReturnType<typeof deferred<DesktopSessionSnapshot | null>>;
    };
    const reads: Read[] = [];
    const api = {
      setSelectedNode: async () => {},
      fetchDesktopSnapshot: async () => snapshot,
      fetchSessionSnapshot: (sessionId: string) => {
        const response = deferred<DesktopSessionSnapshot | null>();
        reads.push({ sessionId, version, response });
        return response.promise;
      },
    };
    const app = testApp({ snapshot, api, selection: { nodeDid, sessionId: "s-1" } });
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance"] });
    const stop = startClientObservation(app, async (listen) => {
      registrations++;
      registration = deferred();
      await registration.promise;
      handler = listen as typeof handler;
      return () => {
        handler = undefined;
      };
    });
    const answer = (differentSession: boolean) => {
      const read = reads.shift();
      expect(read, "session read at its completion checkpoint").toBeDefined();
      if (!arrived) {
        read!.response.reject(
          new BridgeInvokeError({
            code: "projectionPending",
            message: "request pending",
            retryable: true,
            endpoint: null,
          }),
        );
      } else {
        read!.response.resolve(
          sessionSnapshot({
            nodeDid,
            sessionId: differentSession ? "another-session" : read!.sessionId,
            projectionRevision: {
              storeVersion: read!.version,
              provenanceVersion: read!.version,
            },
            timelineItems: [
              userMessage({ itemKey: "notice", content: String(read!.version) }),
            ],
          }),
        );
      }
    };
    try {
      for (const step of trace.steps) {
        switch (step.action) {
          case "Commit":
          case "ArriveWithNotice":
            version++;
            if (step.action === "ArriveWithNotice") arrived = true;
            notice ||= !!handler;
            break;
          case "Handle":
            expect(notice).toBe(true);
            notice = false;
            void handler!({ reason: "store" });
            break;
          case "ArriveSilently":
            arrived = true;
            break;
          case "Select":
            app.stores.selection.setState({ sessionId: `s-${step.after.selected}` });
            break;
          case "ListenRejects":
            registration.reject(new Error("registration rejected"));
            break;
          case "ListenResolves":
            registration.resolve();
            break;
          case "Relistens":
            while (registrations === 1 && vi.getTimerCount())
              await vi.advanceTimersToNextTimerAsync();
            expect(registrations).toBe(2);
            break;
          case "StartupRead":
            break;
          case "StartRead":
            await vi.advanceTimersByTimeAsync(BURST_MS);
            expect(reads).toHaveLength(1);
            break;
          case "FinishMismatch":
          case "FinishRead":
            answer(step.action === "FinishMismatch");
            break;
          case "Stutter":
            break;
          default:
            throw new Error(`unmapped listener action ${step.action}`);
        }
        await vi.advanceTimersByTimeAsync(0);
      }
      while (reads.length || vi.getTimerCount()) {
        if (reads.length) answer(false);
        else await vi.advanceTimersToNextTimerAsync();
        await vi.advanceTimersByTimeAsync(0);
        if (trace.constants.Relisten === false && registrations > 1 && !handler) {
          registration.resolve();
          await vi.advanceTimersByTimeAsync(0);
        }
      }
      const selection = app.stores.selection.getState();
      const { load } = app.stores.session.getState();
      const shown = readSession(app.stores.session);
      const status = projectSessionLoadingStatus({
        selectedSessionId: selection.sessionId,
        selectedNodeDid: nodeDid,
        session: shown,
        sessionLoad: load,
        operationalState: null,
      });
      const exposed =
        load.sessionId === selection.sessionId &&
        (load.phase === "loaded" || !!status?.action);
      expect(exposed, "NoStuckPending").toBe(true);
      expect(
        exposed &&
          (shown?.timelineItems.some(
            (item) => item.kind === "userMessage" && item.content === String(version),
          ) ||
            !!status?.action),
        `ViewConverges: ${JSON.stringify({ version, shown: shown?.projectionRevision, load, registrations, registered: !!handler, status })}`,
      ).toBe(true);
      if (!trace.constants.Relisten) expect(handler).toBeDefined();
    } finally {
      stop();
      vi.useRealTimers();
    }
  },
);
