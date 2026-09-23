/* A task's hooks as rows: when it runs, the command, how long it may take.
   Replaces the canonical JSON area; the document shape is unchanged. */
import { Plus, Trash2 } from "lucide-react";
import type { TaskView } from "@source-inc/gents-desktop-client";

type TaskHook = TaskView["hooks"][number];
type TaskHookPhase = TaskHook["phase"];
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import { StackedRow } from "./rows";

export const PHASES: { value: TaskHookPhase; label: string }[] = [
  { value: "before", label: "Before the run" },
  { value: "after_success", label: "After success" },
  { value: "after_failure", label: "After failure" },
  { value: "finally", label: "Always, afterwards" },
];

/* the draft keeps the command as one line; argv is split on spaces, with
   double quotes keeping an argument together */
export type HookDraft = {
  hook_id: string;
  phase: TaskHookPhase;
  command: string;
  timeout: string;
};

export const toHookDraft = (h: TaskHook): HookDraft => ({
  hook_id: h.hook_id,
  phase: h.phase,
  command: h.command
    .map((a: string) => (/\s/.test(a) ? `"${a.replace(/"/g, '\\"')}"` : a))
    .join(" "),
  timeout: h.timeout_secs == null ? "" : String(h.timeout_secs),
});

export const splitCommand = (line: string): string[] =>
  (line.match(/"(?:[^"\\]|\\.)*"|\S+/g) ?? []).map((a) =>
    a.startsWith('"') ? a.slice(1, -1).replace(/\\"/g, '"') : a,
  );

/* the document hooks, or a message saying what is wrong */
export function hooksFromDraft(rows: HookDraft[]): TaskHook[] | string {
  const ids = new Set<string>();
  const out: TaskHook[] = [];
  for (const [i, r] of rows.entries()) {
    const hook_id = r.hook_id.trim();
    if (!hook_id) return `Hook ${i + 1} needs an ID`;
    if (ids.has(hook_id)) return `Duplicate hook ID: ${hook_id}`;
    ids.add(hook_id);
    const command = splitCommand(r.command);
    if (!command.length) return `Hook ${hook_id} needs a command`;
    let timeout_secs: number | null = null;
    if (r.timeout.trim()) {
      timeout_secs = Number(r.timeout);
      if (!Number.isInteger(timeout_secs) || timeout_secs < 1)
        return `Hook ${hook_id} timeout must be a positive whole number`;
    }
    out.push({ hook_id, phase: r.phase, command, timeout_secs });
  }
  return out;
}

export function HooksRows({
  id,
  value,
  onChange,
  onCommit,
}: {
  id: string;
  value: HookDraft[];
  onChange: (v: HookDraft[]) => void;
  onCommit?: () => void;
}) {
  const set = (i: number, patch: Partial<HookDraft>) =>
    onChange(value.map((h, j) => (j === i ? { ...h, ...patch } : h)));
  return (
    <StackedRow
      label="Hooks"
      description="Commands the runtime runs around the task."
      htmlFor={value.length ? `${id}-0-id` : undefined}
    >
      <div className="grid gap-2">
        {value.map((h, i) => (
          <div
            key={i}
            className="grid grid-cols-[8rem_11rem_minmax(0,1fr)_5rem_auto] items-center gap-2 max-md:grid-cols-1"
          >
            <Input
              id={`${id}-${i}-id`}
              aria-label="Hook ID"
              placeholder="verify"
              className="font-mono text-xs"
              value={h.hook_id}
              onChange={(e) => set(i, { hook_id: e.target.value })}
              onBlur={onCommit}
            />
            <Select
              items={PHASES}
              value={h.phase}
              onValueChange={(v) => {
                if (v) set(i, { phase: v as TaskHookPhase });
                onCommit?.();
              }}
            >
              <SelectTrigger aria-label="When" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {PHASES.map((p) => (
                  <SelectItem key={p.value} value={p.value}>
                    {p.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Input
              aria-label="Command"
              placeholder="cargo test"
              className="font-mono text-xs"
              value={h.command}
              onChange={(e) => set(i, { command: e.target.value })}
              onBlur={onCommit}
            />
            <Input
              aria-label="Timeout in seconds"
              placeholder="secs"
              inputMode="numeric"
              value={h.timeout}
              onChange={(e) => set(i, { timeout: e.target.value })}
              onBlur={onCommit}
            />
            <Button
              variant="quiet"
              size="icon-sm"
              aria-label={`Remove hook ${h.hook_id || i + 1}`}
              onClick={() => {
                onChange(value.filter((_, j) => j !== i));
                onCommit?.();
              }}
            >
              <Trash2 />
            </Button>
          </div>
        ))}
        {value.length === 0 && (
          <p className="text-sm text-muted-foreground">
            No hooks. The task runs on its own.
          </p>
        )}
        <div>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              onChange([
                ...value,
                { hook_id: "", phase: "after_success", command: "", timeout: "" },
              ]);
              onCommit?.();
            }}
          >
            <Plus /> Add hook
          </Button>
        </div>
      </div>
    </StackedRow>
  );
}
