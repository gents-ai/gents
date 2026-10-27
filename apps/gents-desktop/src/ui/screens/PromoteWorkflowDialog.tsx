/* Promote one completed dynamic run into an installed tool: the run's script,
   children and result as `gents job promote` would read them, and a tool
   name. The bridge folds a refusal into one message, its code first. */
import { useEffect, useState } from "react";
import { Button } from "@gents/ui/components/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import { Field, FieldError, FieldLabel } from "@gents/ui/components/field";
import { Input } from "@gents/ui/components/input";
import type {
  DynamicWorkflow,
  WorkflowPromoted,
} from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { errorText } from "../../lib/errorText";

const TOOL_NAME = /^[a-z][a-z0-9_]*$/;
const SCRIPT_LINES = 40;

export function PromoteWorkflowDialog({
  selector,
  onClose,
}: {
  selector: string;
  onClose: (promoted?: WorkflowPromoted) => void;
}) {
  const { actions } = useApp();
  const [run, setRun] = useState<DynamicWorkflow | null>(null);
  const [name, setName] = useState("");
  const [showAll, setShowAll] = useState(false);
  const [busy, setBusy] = useState(false);
  const [promoted, setPromoted] = useState<WorkflowPromoted | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let current = true;
    actions.getDynamicWorkflow(selector).then(
      (loaded) => {
        if (!current) return;
        setRun(loaded);
        setName(TOOL_NAME.test(loaded.view.handle) ? loaded.view.handle : "");
      },
      (e: unknown) => current && setError(errorText(e)),
    );
    return () => {
      current = false;
    };
  }, [actions, selector]);

  async function promote() {
    setBusy(true);
    try {
      setPromoted(await actions.promoteWorkflow({ selector, name, agent: null }));
    } catch (e) {
      setError(errorText(e));
    }
    setBusy(false);
  }

  const lines = run?.script.split("\n") ?? [];
  const collapsed = !showAll && lines.length > SCRIPT_LINES;
  return (
    <Dialog open onOpenChange={(open) => !open && onClose(promoted ?? undefined)}>
      <DialogContent aria-modal="true" className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Promote {run?.view.handle ?? "workflow"}</DialogTitle>
          <DialogDescription>Installs this run's script as a tool.</DialogDescription>
        </DialogHeader>
        {run && (
          <div className="grid gap-4 text-sm">
            <div className="grid gap-1">
              <pre
                data-testid="promote-script"
                className="max-h-96 overflow-auto rounded-md bg-surface p-2 font-mono text-xs"
              >
                {collapsed ? lines.slice(0, SCRIPT_LINES).join("\n") : run.script}
              </pre>
              {lines.length > SCRIPT_LINES && (
                <Button size="sm" variant="ghost" onClick={() => setShowAll(!showAll)}>
                  {showAll ? "Show less" : `Show all ${lines.length} lines`}
                </Button>
              )}
            </div>
            <ul data-testid="promote-children" className="grid gap-0.5">
              {run.view.children.map((child, i) => (
                <li key={i}>
                  {child.title || child.slot}:{" "}
                  <span className="font-mono">{child.target}</span>
                </li>
              ))}
            </ul>
            {run.view.outcome && (
              <p data-testid="promote-result">{run.view.outcome.message}</p>
            )}
            <p data-testid="promote-ceiling">
              Agent ceiling: {run.view.pin.agent_budget}
            </p>
            {run.promoted_as.length > 0 && (
              <p data-testid="promote-already" className="text-sm">
                Already installed as {run.promoted_as.join(", ")}
              </p>
            )}
            {run.promoted_as.length === 0 && (
              <Field>
                <FieldLabel htmlFor="promote-name">Tool name</FieldLabel>
                <Input
                  id="promote-name"
                  value={name}
                  disabled={busy || !!promoted}
                  onChange={(e) => setName(e.target.value)}
                />
                {name && !TOOL_NAME.test(name) && (
                  <FieldError>
                    Lowercase letters, digits and _; starts with a letter.
                  </FieldError>
                )}
              </Field>
            )}
          </div>
        )}
        {promoted && <p className="text-sm">Installed as gents/{name}</p>}
        {error && <FieldError>{error}</FieldError>}
        <DialogFooter>
          <Button variant="ghost" onClick={() => onClose(promoted ?? undefined)}>
            {promoted || (run && run.promoted_as.length > 0) ? "Done" : "Cancel"}
          </Button>
          {run && run.promoted_as.length === 0 && !promoted && !error && (
            <Button
              variant="brand"
              disabled={busy || !TOOL_NAME.test(name)}
              onClick={() => void promote()}
            >
              {busy ? "Promoting…" : "Promote"}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
