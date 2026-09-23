/* A skill is content: a name and instructions you write, not settings you
   pick. So it opens as a sheet beside the behavior, with room to write,
   and resolves with the new skill's id once saved. */
import { useState } from "react";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import {
  Field,
  FieldDescription,
  FieldError,
  FieldLabel,
} from "@gents/ui/components/field";
import { Input } from "@gents/ui/components/input";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetFooter,
  SheetHeader,
  SheetTitle,
} from "@gents/ui/components/sheet";
import { Textarea } from "@gents/ui/components/textarea";
import type { Shell } from "@/hooks/useShell";
import { newId } from "./draft";

const slug = (s: string) =>
  s
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");

export function NewSkillSheet({
  shell,
  deployment,
  open,
  onClose,
}: {
  shell: Shell;
  deployment: DeploymentView;
  open: boolean;
  onClose: (skillId: string | null) => void;
}) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [instructions, setInstructions] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const reset = () => {
    setName("");
    setDescription("");
    setInstructions("");
    setError(null);
  };
  const create = async () => {
    if (!name.trim()) {
      setError("Give the skill a name.");
      return;
    }
    if (!instructions.trim()) {
      setError("Write what the skill does.");
      return;
    }
    setBusy(true);
    setError(null);
    const skillId = newId("skill");
    try {
      await shell.applyConfig((api) =>
        api.saveSkillConfig({
          document: {
            skill_id: skillId,
            agent_did: deployment.agentDid,
            name: slug(name) || skillId,
            description: description.trim() || null,
            instructions: instructions.trim(),
            tool_refs: null,
            display_name: name.trim(),
            enabled: true,
          },
        }),
      );
      reset();
      onClose(skillId);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Sheet
      open={open}
      onOpenChange={(next) => {
        if (!next && !busy) {
          reset();
          onClose(null);
        }
      }}
    >
      <SheetContent
        side="right"
        className="flex w-full flex-col gap-0 border-border/60 max-md:max-w-full data-[side=right]:max-md:w-full md:w-[92vw] data-[side=right]:sm:max-w-lg"
      >
        <SheetHeader>
          <SheetTitle>New skill</SheetTitle>
          <SheetDescription>
            A named set of instructions the behavior can follow. Tool references and the
            interface can be set on the skill afterwards.
          </SheetDescription>
        </SheetHeader>
        <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-4 pb-4">
          <Field>
            <FieldLabel htmlFor="new-skill-name">Name</FieldLabel>
            <Input
              id="new-skill-name"
              autoFocus
              placeholder="Release notes"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
            {name.trim() && <FieldDescription>Saved as {slug(name)}.</FieldDescription>}
          </Field>
          <Field>
            <FieldLabel htmlFor="new-skill-description">Description</FieldLabel>
            <Input
              id="new-skill-description"
              placeholder="When to use it, in one line"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
            />
          </Field>
          <Field className="flex min-h-0 flex-1 flex-col">
            <FieldLabel htmlFor="new-skill-instructions">Instructions</FieldLabel>
            <Textarea
              id="new-skill-instructions"
              rows={14}
              className="min-h-40 flex-1 resize-none font-mono text-xs"
              placeholder="Step by step, as you would tell a colleague."
              value={instructions}
              onChange={(e) => setInstructions(e.target.value)}
            />
          </Field>
          {error && <FieldError>{error}</FieldError>}
        </div>
        <SheetFooter>
          <Button variant="ghost" disabled={busy} onClick={() => onClose(null)}>
            Cancel
          </Button>
          <Button variant="brand" disabled={busy} onClick={create}>
            {busy ? "Creating…" : "Create skill"}
          </Button>
        </SheetFooter>
      </SheetContent>
    </Sheet>
  );
}
