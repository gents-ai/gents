import { useEffect, useState } from "react";
import { Button } from "@gents/ui/components/button";
import type { DynamicWorkflows } from "@source-inc/gents-desktop-client";
import type { JobView } from "@source-inc/gents-desktop-client/generated/JobView";
import { useApp } from "@/app/AppContext";
import { errorText } from "../../../lib/errorText";
import { EmptyList } from "./ListDetail";
import { Group, Row } from "./rows";
import { PromoteWorkflowDialog } from "../PromoteWorkflowDialog";
import { WorkflowRunSheet } from "../WorkflowRunSheet";
import { nodeKeyOf, type NodeView } from "../../../hooks/fleetStore";
import { useBootstrap } from "@/hooks/useClient";
import { NO_SESSIONS, useFleet } from "@/hooks/useFleet";
import { isLocalNode } from "@/lib/firstRun";
import { ACTIVE_STATES, RunningRuns, observedOf, useJobs } from "./WorkflowRuns";

/* the node's running runs, read for any node; then, on the local node, its
   installed workflows and the dynamic scripts it completed, one row per
   script, each promotable from its latest run */
export function WorkflowsPanel({ deployment }: { deployment: NodeView }) {
  const bootstrap = useBootstrap();
  const local = isLocalNode(deployment, bootstrap?.initNodeDid);
  const nodeDid = deployment.nodeDid;
  /* the node's session summaries change with its requests and tool calls;
     the node card re-reads its operations snapshot on the same signal */
  const sessions = useFleet((s) => s.sessionsOf[nodeKeyOf(deployment)] ?? NO_SESSIONS);
  const running = useJobs({ nodeDid, states: ACTIVE_STATES }, sessions);
  const [opened, setOpened] = useState<JobView | null>(null);
  const [sheetOpen, setSheetOpen] = useState(false);
  const open = (run: JobView) => {
    setOpened(run);
    setSheetOpen(true);
  };
  const shownRun = opened
    ? running.runs?.find((run) => run.row_doc_id === opened.row_doc_id)
    : undefined;

  return (
    <div>
      <RunningRuns
        runs={running.runs}
        error={running.error}
        sessions={sessions}
        onOpen={open}
      />
      {local && <LocalWorkflows />}
      {opened && (
        <WorkflowRunSheet
          open={sheetOpen}
          nodeDid={nodeDid}
          rowDocId={opened.row_doc_id}
          observed={observedOf(shownRun)}
          onClose={() => setSheetOpen(false)}
        />
      )}
    </div>
  );
}

/* read from this desktop's own home, so only for the local node */
function LocalWorkflows() {
  const { actions } = useApp();
  const [workflows, setWorkflows] = useState<DynamicWorkflows | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selector, setSelector] = useState<string | null>(null);

  const [loads, setLoads] = useState(0);

  useEffect(() => {
    let current = true;
    actions.listDynamicWorkflows().then(
      (value) => current && setWorkflows(value),
      (e: unknown) => current && setError(errorText(e)),
    );
    return () => {
      current = false;
    };
  }, [actions, loads]);

  if (error) return <p className="text-sm text-destructive">{error}</p>;
  if (!workflows) return null;
  const { installed, workflows: dynamic } = workflows;
  return (
    <>
      {installed.length > 0 && (
        <Group title="Installed">
          {installed.map((w) => (
            <div key={w.plugin} data-testid="installed-workflow-row">
              <Row
                label={w.plugin}
                description={[w.version, w.pack, w.description]
                  .filter(Boolean)
                  .join(" · ")}
              />
            </div>
          ))}
        </Group>
      )}
      {dynamic.length === 0 ? (
        <EmptyList>
          No workflows yet. A workflow an agent writes and runs appears here once it
          completes, to keep as a tool.
        </EmptyList>
      ) : (
        <Group title="Dynamic workflows">
          {dynamic.map((w) => (
            <div key={w.script_id} data-testid="workflow-row">
              <Row
                label={w.handle}
                description={[
                  `${w.runs} ${w.runs === 1 ? "run" : "runs"}`,
                  `last ${new Date(w.last_run_at).toLocaleString()}`,
                  w.agent_did,
                  w.promoted_as.length > 0 && `promoted as ${w.promoted_as.join(", ")}`,
                ]
                  .filter(Boolean)
                  .join(" · ")}
              >
                {w.promoted_as.length === 0 && (
                  <Button
                    variant="outline"
                    onClick={() => setSelector(w.latest_address)}
                  >
                    Promote…
                  </Button>
                )}
              </Row>
            </div>
          ))}
        </Group>
      )}
      {selector && (
        <PromoteWorkflowDialog
          selector={selector}
          onClose={(promoted) => {
            setSelector(null);
            if (promoted) setLoads((count) => count + 1);
          }}
        />
      )}
    </>
  );
}
