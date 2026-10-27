import { useEffect, useState } from "react";
import { Button } from "@gents/ui/components/button";
import type { DynamicWorkflowSummary } from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { Group, Row } from "../agent/rows";
import { PromoteWorkflowDialog } from "../PromoteWorkflowDialog";

/* dynamic scripts this node ran, one row per script, each promotable from
   its latest run */
export function WorkflowsPanel() {
  const { actions } = useApp();
  const [workflows, setWorkflows] = useState<DynamicWorkflowSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selector, setSelector] = useState<string | null>(null);

  const [loads, setLoads] = useState(0);

  useEffect(() => {
    actions.listDynamicWorkflows().then(
      (list) => setWorkflows(list.workflows),
      (e: unknown) => setError(e instanceof Error ? e.message : String(e)),
    );
  }, [actions, loads]);

  if (error) return <p className="text-sm text-destructive">{error}</p>;
  if (!workflows) return null;
  return (
    <div>
      {workflows.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          No dynamic workflows have run yet.
        </p>
      ) : (
        <Group title="Dynamic workflows">
          {workflows.map((w) => (
            <div key={w.script_id} data-testid="workflow-row">
              <Row
                label={w.handle}
                description={[
                  `${w.runs} ${w.runs === 1 ? "run" : "runs"}`,
                  `last ${new Date(w.last_run_at).toLocaleString()}`,
                  w.agent_id,
                  w.promoted_as.length > 0 && `promoted as ${w.promoted_as.join(", ")}`,
                ]
                  .filter(Boolean)
                  .join(" · ")}
              >
                <Button variant="outline" onClick={() => setSelector(w.latest_address)}>
                  Promote…
                </Button>
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
            if (promoted) setLoads(loads + 1);
          }}
        />
      )}
    </div>
  );
}
