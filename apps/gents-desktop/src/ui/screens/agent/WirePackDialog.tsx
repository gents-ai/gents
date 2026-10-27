/* Enable a plugin for one agent. The preview is the writer's own report for
   the chosen agent, so what is shown here is what Enable writes. */
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import type { AgentView, PackWireResult } from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";

export type WirePackDialogProps = {
  /** the plugin to wire, `<namespace>/<name>` */
  plugin: string;
  agents: AgentView[];
  /** the agent the select starts on */
  agent?: string;
  /** content packs whose AgentTargets join the job block */
  targetsFrom: string[];
  onClose: (wired: boolean) => void;
};

const message = (error: unknown) =>
  error instanceof Error ? error.message : String(error);

export function WirePackDialog({
  plugin,
  agents,
  agent,
  targetsFrom,
  onClose,
}: WirePackDialogProps) {
  const { actions } = useApp();
  const [agentId, setAgentId] = useState(agent ?? agents[0]?.agentId ?? "");
  const [preview, setPreview] = useState<PackWireResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const request = { plugin, agent: agentId, targetsFrom, remove: false };

  useEffect(() => {
    if (!agentId) return;
    let current = true;
    setPreview(null);
    setError(null);
    actions
      .wirePack({ ...request, preview: true })
      .then((result) => current && setPreview(result))
      .catch((e) => current && setError(message(e)));
    return () => {
      current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [actions, plugin, agentId, targetsFrom.join("\n")]);

  async function confirm() {
    setBusy(true);
    setError(null);
    try {
      await actions.wirePack({ ...request, preview: false });
      onClose(true);
    } catch (e) {
      setError(message(e));
      setBusy(false);
    }
  }

  const job = preview?.report.job;
  return (
    <Dialog open onOpenChange={(open) => !open && onClose(false)}>
      <DialogContent aria-modal="true" className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Enable {plugin} for an agent</DialogTitle>
          <DialogDescription>Nothing is written until you enable it.</DialogDescription>
        </DialogHeader>
        <div className="grid gap-4">
          <Field>
            <FieldLabel htmlFor="wire-agent">Agent</FieldLabel>
            <Select
              items={agents.map((a) => ({
                value: a.agentId,
                label: a.displayName,
              }))}
              value={agentId}
              onValueChange={(v) => v && setAgentId(v)}
            >
              <SelectTrigger id="wire-agent" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {agents.map((a) => (
                  <SelectItem key={a.agentId} value={a.agentId}>
                    {a.displayName}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>
          {preview && (
            <ul className="grid gap-1 text-sm">
              {job ? (
                <>
                  <li>Targets: {job.agent_targets.join(", ")}</li>
                  <li>Max spawns: {job.max_spawns}</li>
                  <li>Max live: {job.max_live}</li>
                  <li>Dynamic scripts: {job.policy.dynamic}</li>
                  <li>
                    Dynamic targets: {job.policy.dynamic_targets.join(", ") || "none"}
                  </li>
                  <li>
                    Trusted packs: {job.policy.trusted_packs.join(", ") || "none"}
                  </li>
                </>
              ) : (
                preview.report.next_call && <li>{preview.report.next_call}</li>
              )}
              {preview.authority && <li>Authority: {preview.authority}</li>}
              {preview.report.warnings.map((warning) => (
                <li key={warning}>{warning}</li>
              ))}
            </ul>
          )}
          {error && <FieldError>{error}</FieldError>}
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onClose(false)}>
            Cancel
          </Button>
          <Button
            variant="brand"
            disabled={busy || !preview}
            onClick={() => void confirm()}
          >
            Enable
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
