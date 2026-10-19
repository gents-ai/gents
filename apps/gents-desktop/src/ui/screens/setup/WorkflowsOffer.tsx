/* Offer to give the Engineer the workflow engine at the end of setup. */
import { useEffect, useState } from "react";
import { Button } from "@gents/ui/components/button";
import { useApp } from "@/app/AppContext";
import type { NodeView } from "../../../hooks/fleetStore";
import { engineerAgentOf } from "../../lib/engineer";
import { WORKFLOW_ENGINE, WirePackDialog } from "../agent/WirePackDialog";

export function WorkflowsOffer({ deployment }: { deployment: NodeView }) {
  const { actions } = useApp();
  const [installed, setInstalled] = useState<boolean | null>(null);
  const [open, setOpen] = useState(false);
  const [wired, setWired] = useState(false);

  useEffect(() => {
    let current = true;
    actions
      .listPackWireSources()
      .then(
        (sources) =>
          current &&
          setInstalled(sources.plugins.some((p) => p.plugin === WORKFLOW_ENGINE.plugin)),
      )
      .catch(() => {});
    return () => {
      current = false;
    };
  }, [actions]);

  if (installed === null || wired) return null;
  return (
    <>
      <Button variant="outline" onClick={() => setOpen(true)}>
        {installed ? "Enable workflows for the Engineer" : "Install workflows"}
      </Button>
      {open && (
        <WirePackDialog
          plugin={WORKFLOW_ENGINE.plugin}
          agents={deployment.agents}
          agent={engineerAgentOf(deployment)?.agentId}
          targetsFrom={[]}
          installFirst={installed ? undefined : WORKFLOW_ENGINE.pack}
          onClose={(done) => {
            setOpen(false);
            setWired(done);
          }}
        />
      )}
    </>
  );
}
