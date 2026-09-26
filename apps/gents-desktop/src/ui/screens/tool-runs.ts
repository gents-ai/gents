/* A long run of tool calls, folded into the units a person would name.
   Profiling a real export (see scripts/handoff-profile.mjs) found three
   quarters of every transcript is shell commands, runs of one kind
   reaching 228 calls, and the existing per-request grouping leaving 561
   calls in three groups. Folding by kind alone would put 77% of a session
   in one bucket, so a command is folded by its verb instead.

   The rules the folding holds to:
     · fold, never reorder — a run is always contiguous
     · never fold a failure out of sight: a run says how many failed, and
       a run that is nothing but failures is not folded at all
     · fold only what is worth folding, so a pair stays two plain rows */
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";

/* An absolute path inside a checkout is mostly the checkout: every row in a
   real session began with the same forty characters of home directory and
   repo. Keep the tail, which is what tells two rows apart, and let the
   title carry the rest. */
export const shortPath = (path: string, keep = 3): string => {
  /* a machine that separates with a backslash has the same long prefix to
     lose, and a drive letter is as absolute as a leading slash */
  const sep = path.includes("\\") && !path.includes("/") ? "\\" : "/";
  const absolute = path.startsWith("/") || /^[A-Za-z]:[\\/]/.test(path);
  const segments = path.split(/[\\/]/).filter(Boolean);
  if (!absolute || segments.length <= keep) return path;
  return `…${sep}${segments.slice(-keep).join(sep)}`;
};

/* Under this, a run costs a reader more than the rows it saves — and how
   many that is depends on what is being folded. A search row is mostly
   noise: the file it looked in is one of forty, and three of them say
   nothing three times. An edit row is the opposite. It names the file that
   changed, which is the substance of the turn, and there are few of them:
   edits were 3% of calls in the profiled export against 77% commands.
   Folding a pair of edits hides two filenames to save one row. */
const FOLD_FROM: Record<string, number> = {
  looking: 3,
  running: 3,
  /* an edit names the file it changed, which is the substance of the turn
     and there are few of them: folding a pair hides two filenames */
  changing: 5,
};
const foldFrom = (kind: string | null): number => (kind ? (FOLD_FROM[kind] ?? 3) : 3);

const failed = (tool: RenderedToolCallView) =>
  tool.statusKind === "error" || tool.statusKind === "failed" || Boolean(tool.denial);

/* what the agent is doing right now is the one thing a person is watching
   for, so it is never folded away behind a count */
const live = (tool: RenderedToolCallView) => tool.statusKind === "running";

/* `cd <dir> && real-command …` is the common shape, and the cd is
   scaffolding: a profile of a real export found `cd` as the leading token
   of 1,037 of 1,525 commands, against 548 for the most common real verb.
   Step past it, or every run looks like a run of cd. */
/* The parts of a compound command that actually do something. A shell
   call is rarely one verb: `sleep 20; cd … && ls .lake/packages` is three,
   and the first two are scaffolding. Splitting on the separators and
   dropping what only positions the work leaves what the call was for. */
const SCAFFOLDING = new Set([
  "cd",
  "echo",
  "sleep",
  "true",
  "false",
  "set",
  "export",
  "source",
]);

/* Verbs that do something rather than report it. A call containing one of
   these is a run however much looking surrounds it. */
const EXECUTING = new Set([
  /* and the same for what builds, moves or fetches something */
  "dotnet",
  "msbuild",
  "nuget",
  "gradle",
  "mvn",
  "swift",
  "xcodebuild",
  "powershell",
  "pwsh",
  "copy",
  "move",
  "del",
  "ren",
  "new-item",
  "remove-item",
  "copy-item",
  "invoke-webrequest",
  "cargo",
  "rustc",
  "go",
  "node",
  "npm",
  "pnpm",
  "yarn",
  "python",
  "python3",
  "sh",
  "bash",
  "zsh",
  "make",
  "just",
  "lake",
  "curl",
  "wget",
  "ssh",
  "scp",
  "docker",
  "kubectl",
  "terraform",
  "gcloud",
  "aws",
  "mv",
  "cp",
  "rm",
  "mkdir",
  "touch",
  "chmod",
  "ln",
  "tee",
  "kill",
  "pkill",
  "open",
]);

function commandParts(command: string): { verb: string; sub: string | null }[] {
  return (
    command
      .split(/&&|\|\||;/)
      .map((part) => part.trim())
      .filter((part) => part && !/^[A-Za-z_][A-Za-z0-9_]*=/.test(part))
      .map((part) => {
        const words = part.split(/\s+/);
        const head = words[0] ?? "";
        const bare = head.split(/[\\/]/).pop() ?? "";
        const verb = bare.replace(/\.(exe|cmd|bat|ps1)$/i, "");
        const next = words[1] ?? "";
        return {
          verb,
          sub: /^[a-z][a-z0-9-]*$/.test(next) ? next : null,
        };
      })
      /* PowerShell names a verb Get-ChildItem and cmd names one findstr:
       a case-sensitive match on the first sees neither, and a command
       nothing can read is a command that breaks every run it lands in */
      .filter(
        ({ verb }) =>
          /^[A-Za-z][A-Za-z0-9_.-]{0,24}$/.test(verb) && !SCAFFOLDING.has(verb),
      )
  );
}

/* the verb a row is named by: the first that is not scaffolding */
function commandVerb(command: string): string | null {
  return commandParts(command)[0]?.verb ?? null;
}

/* Some verbs only look when their subcommand does. `git status` and
   `git diff` are reading; `git commit` is not. A profiled export had
   sixteen lone `gh` calls, eight `git status`, nine `cargo tree` and six
   `git diff` stranded between sweeps of search, each one breaking the run
   it landed in — they are the same act as the greps around them. */
const READ_ONLY_SUBCOMMANDS: Record<string, Set<string>> = {
  git: new Set([
    "status",
    "diff",
    "log",
    "show",
    "branch",
    "blame",
    "describe",
    "rev-parse",
    "ls-files",
    "ls-tree",
    "remote",
    "worktree",
    "shortlog",
  ]),
  gh: new Set(["issue", "pr", "repo", "api", "run", "release", "search"]),
  cargo: new Set(["tree", "metadata", "search", "locate-project"]),
  docker: new Set(["ps", "images", "logs", "inspect"]),
  npm: new Set(["ls", "view", "outdated"]),
  pnpm: new Set(["ls", "why", "outdated"]),
};

/* Verbs that look rather than change. A real turn alternates them with
   reads one or two at a time — grep, read, grep, sed, read — and folding
   by kind leaves that as forty rows about one activity. What a person saw
   was the agent working out where something lived, so that is the unit. */
const LOOKING = new Set([
  /* cmd and PowerShell say the same things in their own words */
  "dir",
  "type",
  "findstr",
  "where",
  "get-childitem",
  "get-content",
  "select-string",
  "get-item",
  "measure-object",
  "gci",
  "gc",
  "sls",
  "grep",
  "rg",
  "sed",
  "ls",
  "cat",
  "head",
  "tail",
  "find",
  "wc",
  "stat",
  "file",
  "which",
  "awk",
  "sort",
  "uniq",
  "diff",
  "echo",
  "tree",
  "jq",
]);

/* What a call was for, which is what a run of them means. Everything lands
   somewhere: a call with no bucket does not merely fail to fold, it breaks
   whatever run it falls into, and in a profiled export 114 of them did
   exactly that — a third of every unfolded row. A worker is the exception,
   having a fold of its own. */
function classify(tool: RenderedToolCallView): string | null {
  const p = tool.presentation;
  if (p.kind === "subagent" || p.kind === "process") return null;
  if (p.kind === "fileEdit") return "changing";
  if (p.kind === "fileRead") return "looking";
  if (p.kind !== "command") return "running";
  const parts = commandParts(p.command);
  if (parts.length === 0) return "running";
  /* A call is a run if any part of it does something: builds, executes,
     fetches or changes. Otherwise it is looking, even where a part is a
     verb nothing here knows — an unfamiliar reader is still a reader, and
     treating it as a run breaks the sweep it sits in for no reason. */
  return parts.some(({ verb, sub }) => {
    const word = verb.toLowerCase();
    if (LOOKING.has(word)) return false;
    const reads = READ_ONLY_SUBCOMMANDS[word];
    if (reads) return !(sub && reads.has(sub));
    return EXECUTING.has(word);
  })
    ? "running"
    : "looking";
}

function nameVerb(tool: RenderedToolCallView): string | null {
  const p = tool.presentation;
  return p.kind === "command"
    ? commandVerb(p.command)
    : p.kind === "fileRead" || p.kind === "fileEdit"
      ? p.operation
      : p.kind === "mcp"
        ? (p.selectedToolName ?? p.serviceId)
        : (tool.toolName ?? null);
}

/* A call is classified three or four times over a fold — once to bucket
   it, again to name its verb for the tally, again for a label — and each
   pass splits and matches the command string. The answer cannot change for
   a given call, so it is worked out once and kept against the call itself:
   a WeakMap holds it only as long as the transcript does, and a redraw of
   the same calls costs nothing the second time. */
type Reading = { kind: string | null; verb: string | null; target: string | null };
const readings = new WeakMap<RenderedToolCallView, Reading>();

function reading(tool: RenderedToolCallView): Reading {
  const seen = readings.get(tool);
  if (seen) return seen;
  const fresh = {
    kind: classify(tool),
    verb: nameVerb(tool),
    target: nameTarget(tool),
  };
  readings.set(tool, fresh);
  return fresh;
}

/* what a call acted on, where it names one thing */
function nameTarget(tool: RenderedToolCallView): string | null {
  const p = tool.presentation;
  return p.kind === "fileEdit" ? p.path : p.kind === "fileRead" ? p.target : null;
}

/* a run that worked on one file the whole way through */
function oneTarget(tools: RenderedToolCallView[]): string | null {
  const first = reading(tools[0]!).target;
  if (!first) return null;
  return tools.every((t) => reading(t).target === first) ? first : null;
}

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

function label(
  tool: RenderedToolCallView,
  count: number,
  target?: string | null,
): string {
  /* the same file over and over is one act on one file, and its name is
     the thing worth keeping: 'edited build.rs ×4' rather than four rows
     that each say build.rs, or a count that says which file no longer */
  if (target) {
    const kind = reading(tool).kind;
    const verb = kind === "changing" ? "edited" : kind === "looking" ? "read" : "ran";
    return `${verb} ${shortPath(target)} ×${count}`;
  }
  switch (reading(tool).kind) {
    case "looking":
      return `looked through ${plural(count, "file", "files")}`;
    case "changing":
      return `changed ${plural(count, "file", "files")}`;
    case "running":
      return `ran ${plural(count, "command", "commands")}`;
    default:
      return `${count} steps`;
  }
}

/* what a run was made of, counted over the whole of it rather than by
   adjacency: a real agent interleaves its greps and seds one or two at a
   time, so folding the inside by run again gives 118 fragments for 228
   calls, which is not a summary of anything. A tally is. */
type RunTally = { label: string; count: number };

export type ToolRun =
  | { kind: "one"; tool: RenderedToolCallView }
  | {
      kind: "run";
      key: string;
      label: string;
      tools: RenderedToolCallView[];
      tally: RunTally[];
      failures: number;
    };

/* the whole point of opening a run is usually the one thing that went
   wrong in it, so the run says which */
export function firstFailure(
  tools: RenderedToolCallView[],
): RenderedToolCallView | null {
  return tools.find(failed) ?? null;
}

function tallyOf(tools: RenderedToolCallView[]): RunTally[] {
  const counts = new Map<string, number>();
  for (const tool of tools) {
    const verb = reading(tool).verb ?? "other";
    counts.set(verb, (counts.get(verb) ?? 0) + 1);
  }
  return [...counts]
    .map(([label, count]) => ({ label, count }))
    .sort((a, b) => b.count - a.count || a.label.localeCompare(b.label));
}

function foldRuns(tools: RenderedToolCallView[]): ToolRun[] {
  const out: ToolRun[] = [];
  let run: RenderedToolCallView[] = [];
  let key: string | null = null;

  const flush = () => {
    if (run.length === 0) return;
    const failures = run.filter(failed).length;
    const target = oneTarget(run);
    /* a run of nothing but failures is the thing to read, not to fold; a
       run that never left one file folds from two, because the second row
       adds a repetition rather than a fact */
    if (run.length < (target ? 2 : foldFrom(key)) || failures === run.length)
      for (const tool of run) out.push({ kind: "one", tool });
    else
      out.push({
        kind: "run",
        key: `${key}:${run[0]!.itemKey}`,
        label: label(run[0]!, run.length, oneTarget(run)),
        tools: run,
        tally: tallyOf(run),
        failures,
      });
    run = [];
    key = null;
  };

  /* One call of another kind in the middle of a sweep is a step aside, not
     the end of it: reading a file to decide what to grep for next does not
     make two searches out of one. A single interloper is carried inside the
     run — it still appears, in its place, when the run is opened — and two
     in a row end the run as before. Its own kind still leads the tally, so
     nothing is folded away silently. */
  let held: RenderedToolCallView | null = null;
  for (const tool of tools) {
    const next = live(tool) ? null : reading(tool).kind;
    if (next === null) {
      flush();
      out.push({ kind: "one", tool });
      continue;
    }
    if (next !== key && key !== null) {
      /* A read in the middle of a sweep is a step aside — the agent looked
         at one file to decide what to search for next. A build is not. It
         is the thing that happened, and folding it into "looked through
         seven files" hides the only row anyone was looking for. */
      if (next !== "running" && held === null && run.length >= foldFrom(key)) {
        /* wait one call to see whether the sweep resumes */
        held = tool;
        continue;
      }
      flush();
      if (held) out.push({ kind: "one", tool: held });
      held = null;
      key = next;
      run.push(tool);
      continue;
    }
    if (held) {
      /* the sweep resumed: the interloper belongs inside it */
      run.push(held);
      held = null;
    }
    key = next;
    run.push(tool);
  }
  if (held) {
    flush();
    out.push({ kind: "one", tool: held });
  } else flush();
  return out;
}

/* A worker's life, gathered.

   The other folds are contiguous: they never move a row past another,
   because a transcript is a sequence and rearranging it would lie about
   what happened. This one is different on purpose. A worker's steps are
   scattered through the turn — started here, messaged twice there — and
   read as a dozen unrelated rows about five workers. Gathering them is not a claim about when they happened; the
   group sits where the worker first appears, and its steps keep their
   order inside it.

   Only a worker with more than one step is gathered. One start and
   nothing else is already a row about one thing. */
type WorkerRun = {
  kind: "worker";
  key: string;
  tools: RenderedToolCallView[];
};

export type TranscriptRun = ToolRun | WorkerRun;

const workerKey = (tool: RenderedToolCallView) => {
  const p = tool.presentation;
  if (p.kind !== "subagent") return null;
  return p.sessionId ?? p.name ?? null;
};

export function foldWorkers(tools: RenderedToolCallView[]): TranscriptRun[] {
  const byWorker = new Map<string, RenderedToolCallView[]>();
  for (const tool of tools) {
    const key = workerKey(tool);
    if (!key) continue;
    if (!byWorker.has(key)) byWorker.set(key, []);
    byWorker.get(key)!.push(tool);
  }
  const gathered = new Set(
    [...byWorker].filter(([, its]) => its.length > 1).map(([key]) => key),
  );
  const placed = new Set<string>();
  const out: TranscriptRun[] = [];
  /* everything that is not a gathered worker keeps its own place, and the
     rest of the folding runs over those in the gaps */
  let loose: RenderedToolCallView[] = [];
  const flushLoose = () => {
    if (loose.length) out.push(...foldRuns(loose));
    loose = [];
  };
  for (const tool of tools) {
    const key = workerKey(tool);
    if (key && gathered.has(key)) {
      flushLoose();
      if (placed.has(key)) continue;
      placed.add(key);
      out.push({ kind: "worker", key, tools: byWorker.get(key)! });
      continue;
    }
    loose.push(tool);
  }
  flushLoose();
  return out;
}

/* what happened to a worker, in the order it happened */
export function workerStory(tools: RenderedToolCallView[]): string {
  const counts = new Map<string, number>();
  for (const tool of tools) {
    const p = tool.presentation;
    if (p.kind !== "subagent") continue;
    const verb =
      p.action === "start" ? "started" : p.action === "message" ? "messaged" : p.action;
    counts.set(verb, (counts.get(verb) ?? 0) + 1);
  }
  return [...counts].map(([verb, n]) => (n > 1 ? `${verb} ×${n}` : verb)).join(" · ");
}
