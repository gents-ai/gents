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

/* The draft shows the command as one line. The stored argv is kept beside
   it and written back untouched unless that row's command text is edited,
   so a save never reshapes a hook nobody changed. An edited line is parsed
   with a lossless scheme: an argument that is empty or holds whitespace, a
   quote or a backslash is written in double quotes with \\ and \" escaped;
   outside quotes every character is literal, so a typed Windows path keeps
   its backslashes. parse(format(argv)) is argv for every argv. */
export type HookDraft = {
  hook_id: string;
  phase: TaskHookPhase;
  command: string;
  timeout: string;
  /* the argv as stored, and the line it was shown as */
  stored?: { argv: string[]; line: string } | null;
};

const needsQuotes = (a: string) => a === "" || /[\s"'\\]/.test(a);

export const formatCommand = (argv: string[]): string =>
  argv
    .map((a) => (needsQuotes(a) ? `"${a.replace(/[\\"]/g, (c) => `\\${c}`)}"` : a))
    .join(" ");

export function splitCommand(line: string): string[] {
  const out: string[] = [];
  let current = "";
  let inToken = false;
  let i = 0;
  while (i < line.length) {
    const c = line[i]!;
    if (/\s/.test(c)) {
      if (inToken) out.push(current);
      current = "";
      inToken = false;
      i += 1;
    } else if (c === '"') {
      inToken = true;
      i += 1;
      while (i < line.length && line[i] !== '"') {
        if (line[i] === "\\" && (line[i + 1] === '"' || line[i + 1] === "\\")) {
          current += line[i + 1];
          i += 2;
        } else {
          current += line[i];
          i += 1;
        }
      }
      i += 1; /* the closing quote, or the end of an unterminated one */
    } else {
      inToken = true;
      current += c;
      i += 1;
    }
  }
  if (inToken) out.push(current);
  return out;
}

export const toHookDraft = (h: TaskHook): HookDraft => {
  const line = formatCommand(h.command);
  return {
    hook_id: h.hook_id,
    phase: h.phase,
    command: line,
    timeout: h.timeout_secs == null ? "" : String(h.timeout_secs),
    stored: { argv: [...h.command], line },
  };
};

/* the argv to write: the stored one while its line is untouched */
export const argvOf = (r: HookDraft): string[] =>
  r.stored && r.command === r.stored.line ? r.stored.argv : splitCommand(r.command);

/* the document hooks, or a message saying what is wrong */
export function hooksFromDraft(rows: HookDraft[]): TaskHook[] | string {
  const ids = new Set<string>();
  const out: TaskHook[] = [];
  for (const [i, r] of rows.entries()) {
    const hook_id = r.hook_id.trim();
    if (!hook_id) return `Hook ${i + 1} needs an ID`;
    if (ids.has(hook_id)) return `Duplicate hook ID: ${hook_id}`;
    ids.add(hook_id);
    const command = argvOf(r);
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
