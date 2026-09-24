/* "New task…", "New schedule…" and "New event source…" on a trigger. The
   dialog only drafts the document: nothing is written when it opens, is
   cancelled or is confirmed. The trigger's own Save writes the new document
   and the trigger together, so a trigger that is never saved leaves
   nothing behind. A new task starts disabled, and an event source has no
   default collection (watching AgentRequest would let a task trigger
   itself). */
import { useState } from "react";
import type {
  DeploymentView,
  EventSource,
  Schedule,
} from "@source-inc/gents-desktop-client";
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import { Textarea } from "@gents/ui/components/textarea";
import { cadenceInWords, eventInWords } from "./automation";
import { newId, validateCronSchedule } from "./draft";
import { PRESETS, TZ, type Task } from "./NewAutomationDialog";

export type NewReference =
  | { kind: "task"; document: Task }
  | { kind: "schedule"; document: Schedule }
  | { kind: "event"; document: EventSource };

const TITLES = {
  task: "New task",
  schedule: "New schedule",
  event: "New event source",
} as const;

export function NewReferenceDialog({
  deployment,
  kind,
  onClose,
}: {
  deployment: DeploymentView;
  /* which document to draft; null when closed */
  kind: NewReference["kind"] | null;
  /* the drafted document, or null when cancelled */
  onClose: (drafted: NewReference | null) => void;
}) {
  const agent_did = deployment.agentDid;
  const [name, setName] = useState("");
  const [prompt, setPrompt] = useState("");
  const [behaviorId, setBehaviorId] = useState(
    deployment.behaviors.find((b) => b.isDefault)?.behaviorId ??
      deployment.behaviors[0]?.behaviorId ??
      "",
  );
  const [preset, setPreset] = useState<(typeof PRESETS)[number]["id"]>("daily");
  const [cron, setCron] = useState("");
  const [collection, setCollection] = useState("");
  const [eventKind, setEventKind] = useState("created");
  const [error, setError] = useState<string | null>(null);
  const finish = (drafted: NewReference | null) => {
    setName("");
    setPrompt("");
    setPreset("daily");
    setCron("");
    setCollection("");
    setEventKind("created");
    setError(null);
    onClose(drafted);
  };
  const confirm = () => {
    try {
      if (kind === "task") {
        if (!prompt.trim()) throw new Error("Say what the agent should do.");
        if (!behaviorId) throw new Error("Choose the behavior that runs it.");
        finish({
          kind,
          document: {
            agent_did,
            task_id: newId("task"),
            display_name: name.trim() || prompt.trim().split("\n")[0].slice(0, 60),
            description: null,
            behavior_id: behaviorId,
            prompt_template: prompt.trim(),
            goal_objective_template: null,
            goal_token_budget: null,
            enabled: false,
            output_schema_ref: null,
          },
        });
      } else if (kind === "schedule") {
        const expression =
          preset === "custom" ? cron : PRESETS.find((p) => p.id === preset)!.cron;
        const schedule_id = newId("sched");
        const cadence = {
          kind: "cron" as const,
          ...validateCronSchedule(expression, TZ),
          missed_run_policy: "latest_only" as const,
        };
        finish({
          kind,
          document: {
            agent_did,
            schedule_id,
            display_name:
              name.trim() || cadenceInWords({ agent_did, schedule_id, cadence }),
            cadence,
          },
        });
      } else if (kind === "event") {
        if (!collection.trim()) throw new Error("Name the collection to watch.");
        const document = {
          agent_did,
          event_source_id: newId("evsrc"),
          display_name: null as string | null,
          source_collection: collection.trim(),
          event_kind: eventKind.trim() || null,
        };
        document.display_name = name.trim() || eventInWords(document);
        finish({ kind, document });
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  return (
    <Dialog open={kind !== null} onOpenChange={(open) => !open && finish(null)}>
      <DialogContent aria-modal="true" className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{kind ? TITLES[kind] : ""}</DialogTitle>
          <DialogDescription>
            Nothing is saved until you save the trigger.
          </DialogDescription>
        </DialogHeader>
        <div className="grid gap-4">
          <Field>
            <FieldLabel htmlFor="new-ref-name">Name</FieldLabel>
            <Input
              id="new-ref-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </Field>
          {kind === "task" && (
            <>
              <Field>
                <FieldLabel htmlFor="new-ref-prompt">Prompt</FieldLabel>
                <Textarea
                  id="new-ref-prompt"
                  rows={4}
                  value={prompt}
                  onChange={(e) => setPrompt(e.target.value)}
                />
              </Field>
              <Field>
                <FieldLabel htmlFor="new-ref-behavior">Who</FieldLabel>
                <Select
                  items={deployment.behaviors.map((b) => ({
                    value: b.behaviorId,
                    label: b.displayName,
                  }))}
                  value={behaviorId}
                  onValueChange={(v) => v && setBehaviorId(v)}
                >
                  <SelectTrigger id="new-ref-behavior" className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {deployment.behaviors.map((b) => (
                      <SelectItem key={b.behaviorId} value={b.behaviorId}>
                        {b.displayName}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </Field>
            </>
          )}
          {kind === "schedule" && (
            <Field>
              <FieldLabel htmlFor="new-ref-preset">Cadence</FieldLabel>
              <Select
                items={PRESETS.map((p) => ({ value: p.id, label: p.label }))}
                value={preset}
                onValueChange={(v) => v && setPreset(v as typeof preset)}
              >
                <SelectTrigger id="new-ref-preset" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {PRESETS.map((p) => (
                    <SelectItem key={p.id} value={p.id}>
                      {p.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {preset === "custom" && (
                <Input
                  aria-label="Cron expression"
                  className="mt-2 font-mono"
                  value={cron}
                  onChange={(e) => setCron(e.target.value)}
                />
              )}
            </Field>
          )}
          {kind === "event" && (
            <div className="grid grid-cols-2 gap-3 max-md:grid-cols-1">
              <Field>
                <FieldLabel htmlFor="new-ref-collection">Collection</FieldLabel>
                <Input
                  id="new-ref-collection"
                  className="font-mono"
                  placeholder="MailboxItem"
                  value={collection}
                  onChange={(e) => setCollection(e.target.value)}
                />
              </Field>
              <Field>
                <FieldLabel htmlFor="new-ref-event-kind">Event kind</FieldLabel>
                <Input
                  id="new-ref-event-kind"
                  className="font-mono"
                  value={eventKind}
                  onChange={(e) => setEventKind(e.target.value)}
                />
              </Field>
            </div>
          )}
          {error && <FieldError>{error}</FieldError>}
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={() => finish(null)}>
            Cancel
          </Button>
          <Button variant="brand" onClick={confirm}>
            Add
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
