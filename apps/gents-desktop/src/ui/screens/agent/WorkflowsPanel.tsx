import { useEffect, useState } from "react";
import { Button } from "@gents/ui/components/button";
import type { DynamicWorkflows } from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { errorText } from "../../../lib/errorText";
import { Group, Row } from "./rows";
import { PromoteWorkflowDialog } from "../PromoteWorkflowDialog";

/* dynamic scripts this node completed, one row per script, each promotable from
   its latest run */
export function WorkflowsPanel() {
  const { actions } = useApp();
  const [workflows, setWorkflows] = useState<DynamicWorkflows | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selector, setSelector] = useState<string | null>(null);

  const [loads, setLoads] = useState(0);

  useEffect(() => {
    actions
      .listDynamicWorkflows()
      .then(setWorkflows, (e: unknown) => setError(errorText(e)));
  }, [actions, loads]);

  if (error) return <p className="text-sm text-destructive">{error}</p>;
  if (!workflows) return null;
  const { installed, workflows: dynamic } = workflows;
  return (
    <div>
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
        <p className="text-sm text-muted-foreground">
          No dynamic workflow has completed yet.
        </p>
      ) : (
        <Group title="Dynamic workflows">
          {dynamic.map((w) => (
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
            if (promoted) setLoads(loads + 1);
          }}
        />
      )}
    </div>
  );
}
