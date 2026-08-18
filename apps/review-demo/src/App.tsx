import { useEffect, useMemo, useState } from "react";

import { ReviewDag } from "./graph/ReviewDag.tsx";
import { projectReviewGraph } from "./graph/projectReviewGraph.ts";
import { formatTokenTotals, tokenTotalsForRun } from "./graph/tokenTotals.ts";
import type { GraphNode, ReviewSnapshot } from "./graph/types.ts";
import { SessionDrawer } from "./live/SessionDrawer.tsx";
import {
  loadSnapshot,
  probeHealth,
  type RuntimeHealth,
} from "./live/pollRuntime.ts";
import { EnablingFeatures } from "./talk/EnablingFeatures.tsx";
import { WhatWeWillSee } from "./talk/WhatWeWillSee.tsx";

const EMPTY: ReviewSnapshot = {
  jobs: [],
  areas: [],
  candidates: [],
  scans: [],
  verdicts: [],
  summaries: [],
  findings: [],
  reports: [],
  requests: [],
  calls: [],
};

export function App() {
  const [health, setHealth] = useState<RuntimeHealth>("offline");
  const [snapshot, setSnapshot] = useState<ReviewSnapshot>(EMPTY);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [followedRunId, setFollowedRunId] = useState<string | null>(null);
  const seenJobIds = useMemo(() => new Set<string>(), []);

  useEffect(() => {
    let cancelled = false;
    const healthTick = async () => {
      const up = await probeHealth();
      if (cancelled) {
        return;
      }
      setHealth((current) => {
        if (!up) {
          return "offline";
        }
        return current === "query-failed" ? current : "ready";
      });
    };
    void healthTick();
    const timer = window.setInterval(() => {
      void healthTick();
    }, 1000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, []);

  useEffect(() => {
    if (health === "offline") {
      return;
    }
    let cancelled = false;
    const tick = async () => {
      try {
        const next = await loadSnapshot();
        if (!cancelled) {
          const known = seenJobIds;
          const fresh = next.jobs
            .map((job) => job.run_id)
            .filter((runId) => runId && !known.has(runId));
          for (const runId of next.jobs.map((job) => job.run_id)) {
            known.add(runId);
          }
          if (fresh.length === 1 && known.size > fresh.length) {
            setFollowedRunId(fresh[0]!);
          }
          setSnapshot(next);
          setHealth("ready");
        }
      } catch {
        if (!cancelled) {
          setHealth("query-failed");
        }
      }
    };
    void tick();
    const timer = window.setInterval(() => {
      void tick();
    }, 1500);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [health === "offline"]);

  const urlRunId = useMemo(() => {
    const params = new URLSearchParams(window.location.search);
    return params.get("run");
  }, []);
  const pinnedRunId = urlRunId ?? followedRunId;
  const graph = useMemo(
    () => projectReviewGraph(snapshot, { pinnedRunId }),
    [pinnedRunId, snapshot],
  );
  const selected = graph.nodes.find((node) => node.id === selectedId) ?? null;
  const tokens = useMemo(
    () => tokenTotalsForRun(snapshot.calls, snapshot.requests, graph.runId),
    [graph.runId, snapshot.calls, snapshot.requests],
  );

  const status = statusLine(health, graph.runId);

  return (
    <div className="stage">
      <header className="stage-bar">
        <strong>Gents review</strong>
        <span className="status">
          <span className={`dot ${health === "ready" ? "on" : ""}`} />
          {status}
        </span>
      </header>
      <div className="stage-body">
        <aside className="pane rail">
          <WhatWeWillSee />
          <EnablingFeatures />
        </aside>
        <main className="pane dag-pane">
          <div className="live-head">
            <p className="eyebrow">Live run</p>
            <p className="token-totals">{formatTokenTotals(tokens)}</p>
          </div>
          {snapshot.jobs.length > 1 ? (
            <div className="run-chips">
              {snapshot.jobs.map((job) => (
                <button
                  key={job.run_id}
                  type="button"
                  className={`run-chip${graph.runId === job.run_id ? " on" : ""}`}
                  onClick={() => setFollowedRunId(job.run_id)}
                >
                  {job.run_id}
                </button>
              ))}
            </div>
          ) : null}
          <ReviewDag
            graph={graph}
            selectedId={selectedId}
            onSelect={(node: GraphNode) => setSelectedId(node.id)}
          />
        </main>
        <aside className="pane session-pane">
          <SessionDrawer node={selected} snapshot={snapshot} />
        </aside>
      </div>
    </div>
  );
}

function statusLine(health: RuntimeHealth, runId: string | null): string {
  if (health === "offline") {
    return "waiting for runtime";
  }
  if (health === "query-failed") {
    return "query failed";
  }
  if (!runId) {
    return "19191 ready · waiting for ReviewJob";
  }
  return `19191 ready · ${runId}`;
}
