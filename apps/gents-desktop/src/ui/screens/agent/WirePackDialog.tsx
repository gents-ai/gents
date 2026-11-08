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
import { useCopied } from "@/lib/clipboard";

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

/* The runtime won't pin a plugin through GraphQL while it serves the node's
   home, because the commit's outcome could be unknown; its refusal names
   the command to run with the node stopped. */
const WHILE_RUNNING = "cannot publish a plugin pin through GraphQL";
export const refusedWhileRunning = (error: string) =>
  error.includes(WHILE_RUNNING) ? (error.match(/`([^`]+)`/)?.[1] ?? null) : undefined;

const shellArgument = (value: string) => `'${value.replace(/'/g, "'\\''")}'`;
const wireCommand = (plugin: string, agent: string, targetsFrom: string[]) =>
  [
    "gents pack wire",
    shellArgument(plugin),
    "--agent",
    shellArgument(agent),
    ...targetsFrom.flatMap((target) => ["--targets-from", shellArgument(target)]),
    "--home 'NODE_HOME'",
  ].join(" ");

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
  const [refused, setRefused] = useState(false);
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
      const text = message(e);
      if (refusedWhileRunning(text) !== undefined) {
        setRefused(true);
      } else setError(text);
      setBusy(false);
    }
  }

  const job = preview?.report.job;
  return (
    <Dialog open onOpenChange={(open) => !open && onClose(false)}>
      <DialogContent className="sm:max-w-lg">
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
          {refused ? (
            <Refusal
              plugin={plugin}
              command={wireCommand(plugin, agentId, targetsFrom)}
            />
          ) : (
            error && (
              <FieldError className="[overflow-wrap:anywhere]">{error}</FieldError>
            )
          )}
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => onClose(false)}>
            Cancel
          </Button>
          <Button
            variant="brand"
            disabled={busy || !preview || refused}
            onClick={() => void confirm()}
          >
            Enable
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/* why Enable can't work on a running node yet, with the command to run once
   it's stopped */
function Refusal({ plugin, command }: { plugin: string; command: string }) {
  const { copied, copy } = useCopied();
  return (
    <div role="alert" className="grid gap-2 rounded-xl bg-raised/50 p-3 text-sm">
      <p>
        {plugin} can’t be enabled from the app while this node is running yet. Until it
        can, stop the node, replace NODE_HOME with its home directory, then run this in
        a POSIX shell:
      </p>
      <div className="relative">
        <pre className="rounded-lg bg-background/70 p-2.5 pr-16 font-mono text-xs break-all whitespace-pre-wrap">
          {command}
        </pre>
        <button
          type="button"
          onClick={() => copy(command)}
          className="absolute top-1.5 right-2 rounded-md px-2 py-1 text-xs text-muted-foreground hover:text-foreground"
        >
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
    </div>
  );
}
