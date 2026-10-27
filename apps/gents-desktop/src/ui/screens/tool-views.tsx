import { lazy, Suspense, useRef } from "react";
import { shortPath } from "@/lib/paths";
import { useHomeFolder } from "@/hooks/useHomeFolder";
import type {
  RenderedToolCallView,
  ToolDiffLineView,
} from "@source-inc/gents-desktop-client";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { cn } from "@gents/ui/lib/utils";
import { Button } from "@gents/ui/components/button";
import { toast } from "sonner";
import { revealInFolder } from "../../lib/nativeShell";
import { revealInFolderLabel } from "../../lib/shellPlatform";
import { useFollowNewest } from "../lib/scroll";
import { useHomeDid, useSelectedNode } from "../hooks/useClient";
import { isWorkingNode } from "../lib/nodes";
import { CopyButton } from "./Markdown";
import { JobProgressCell } from "./JobProgressCell";
import {
  diffText,
  duration,
  isAbsolutePath,
  lineCount,
  toolSummary,
} from "./tool-summary";

const pretty = (value: string) => {
  try {
    return JSON.stringify(JSON.parse(value), null, 2);
  } catch {
    return value;
  }
};

const SAID_BY_ICON = new Set(["read", "edited", "$"]);

export function ToolSummary({
  tool,
  className,
  withIcon = false,
}: {
  tool: RenderedToolCallView;
  className?: string;
  withIcon?: boolean;
}) {
  const s = toolSummary(tool);
  const kind = withIcon && SAID_BY_ICON.has(s.kind) ? "" : s.kind;
  return (
    <span className={cn("flex min-w-0 items-baseline gap-2 text-sm", className)}>
      {kind && <span className="shrink-0 text-muted-foreground">{kind}</span>}
      <span
        title={s.full}
        className={cn("min-w-0 truncate", s.mono && "font-mono text-[13px]")}
      >
        {s.primary}
      </span>
      {s.secondary && (
        <span className="min-w-0 max-w-[50%] shrink truncate text-xs text-muted-foreground">
          {s.secondary}
        </span>
      )}
    </span>
  );
}

function Payload({
  label,
  value,
  counted = false,
}: {
  label: string;
  value?: string | null;
  counted?: boolean;
}) {
  if (!value?.trim()) return null;
  const text = pretty(value);
  const lines = counted ? lineCount(text) : 0;
  return (
    <div className="grid grid-cols-[minmax(0,1fr)] gap-1">
      <div className="flex items-center justify-between">
        <span className="font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
          {label}
          {lines > 1 && (
            <span className="normal-case"> · {lines.toLocaleString()} lines</span>
          )}
        </span>
        <CopyButton getText={() => text} />
      </div>
      <ScrollArea
        orientation="both"
        className="max-h-72 rounded-md bg-surface [&_[data-slot=scroll-area-viewport]]:max-h-[inherit]"
      >
        <pre className="w-max min-w-full px-3 py-2 font-mono text-[12px] leading-[18px] text-foreground">
          {text}
        </pre>
      </ScrollArea>
    </div>
  );
}

function Meta({ items }: { items: (string | null | undefined)[] }) {
  const shown = items.filter(Boolean) as string[];
  if (!shown.length) return null;
  return (
    <div className="flex flex-wrap gap-x-3 gap-y-1 font-mono text-[11px] text-muted-foreground">
      {shown.map((m) => (
        <span key={m}>{m}</span>
      ))}
    </div>
  );
}

const GUTTER: Record<ToolDiffLineView["kind"], string> = {
  added: "+",
  removed: "\u2212",
  context: " ",
};
const TONE: Record<ToolDiffLineView["kind"], string> = {
  added: "bg-diff-added text-diff-added-fg",
  removed: "bg-diff-removed text-diff-removed-fg",
  context: "text-muted-foreground",
};

/** Diff lines without highlighting, for while the highlighted view loads
    or when it cannot. */
export function DiffLines({
  lines,
  className,
}: {
  lines: readonly ToolDiffLineView[];
  className?: string;
}) {
  return (
    <ScrollArea
      orientation="both"
      className={cn(
        "max-h-72 rounded-md bg-surface [&_[data-slot=scroll-area-viewport]]:max-h-[inherit]",
        className,
      )}
    >
      <pre className="w-max min-w-full py-2 font-mono text-[12px] leading-[18px]">
        {lines.map((line, i) => (
          <span
            key={i}
            data-diff={line.kind}
            className={cn("block px-3", TONE[line.kind])}
          >
            <span
              aria-hidden="true"
              className="mr-2 inline-block w-2 font-medium opacity-70 select-none"
            >
              {GUTTER[line.kind]}
            </span>
            {line.text}
          </span>
        ))}
      </pre>
    </ScrollArea>
  );
}

const InlineDiff = lazy(() => import("./InlineDiff"));

function DiffBody({ path, diff }: { path: string; diff: readonly ToolDiffLineView[] }) {
  return (
    <div className="relative">
      <CopyButton
        getText={() => diffText([...diff])}
        className="absolute top-1 right-1 z-10"
      />
      <Suspense fallback={<DiffLines lines={diff} />}>
        <InlineDiff path={path} lines={diff} />
      </Suspense>
    </div>
  );
}

export function ToolBody({ tool }: { tool: RenderedToolCallView }) {
  const p = tool.presentation;
  const live = tool.statusKind === "running" ? tool.partialOutputTail : null;
  const denial = tool.statusKind === "error" ? tool.denial : null;
  return (
    <div className="grid gap-3">
      {(tool.jobProgress || tool.jobState) && <JobProgressCell tool={tool} />}
      {denial && (
        <>
          <Meta
            items={[
              `rule · ${denial.ruleId}`,
              denial.deniedCommand && `command · ${denial.deniedCommand}`,
              denial.deniedSubcommand && `subcommand · ${denial.deniedSubcommand}`,
              denial.deniedArgument && `argument · ${denial.deniedArgument}`,
            ]}
          />
          <p className="text-sm">{denial.reasonLine}</p>
          <Payload label="diagnostic" value={denial.diagnostic} />
        </>
      )}
      {tool.cancelCause && (
        <Meta
          items={[
            `canceled · ${tool.cancelCause.cause}`,
            tool.cancelCause.source,
            tool.cancelCause.at
              ? new Date(tool.cancelCause.at).toLocaleTimeString()
              : null,
          ]}
        />
      )}
      <Meta
        items={[
          tool.awaitMode && `await: ${tool.awaitMode}`,
          tool.deadlineAt && `deadline: ${tool.deadlineAt}`,
        ]}
      />
      {p.kind === "command" && (
        <>
          <Meta
            items={[
              p.durationMs != null ? duration(p.durationMs) : null,
              p.cwd,
              p.executionMode && `sandbox: ${p.executionMode}`,
              p.networkMode && `network: ${p.networkMode}`,
            ]}
          />
          <Payload label="stdout" value={p.stdout} counted />
          <Payload label="stderr" value={p.stderr} counted />
          <Payload label="output" value={p.fallbackOutput} counted />
        </>
      )}
      {p.kind === "fileRead" && (
        <>
          <Payload label="contents" value={p.body} />
          <Payload label="output" value={p.fallbackOutput} />
        </>
      )}
      {p.kind === "fileEdit" && (
        <>
          {p.diff.length > 0 && <DiffBody diff={p.diff} path={p.path ?? "file"} />}
          {(p.revealPath || p.path) && tool.statusKind === "success" && (
            <RevealFile path={p.revealPath || p.path!} />
          )}
          <Payload label="output" value={p.fallbackOutput} />
        </>
      )}
      {p.kind === "agent" && (
        <>
          <Payload
            label={p.action === "start" ? "assignment" : "message"}
            value={p.description}
          />
          {p.sessionId && <Meta items={[`session · ${p.sessionId}`]} />}
          <Payload label="result" value={p.output} />
        </>
      )}
      {p.kind === "process" && (
        <>
          <Payload label="arguments" value={p.description} />
          <Payload label="result" value={p.output} />
        </>
      )}
      {p.kind === "mcp" && (
        <>
          <Payload label="arguments" value={p.arguments} />
          <Payload label="result" value={p.output} />
        </>
      )}
      {p.kind === "generic" && (
        <>
          <Payload label="input" value={p.input} />
          <Payload label="result" value={p.output} />
        </>
      )}
      {live && <LiveOutput subject={tool.itemKey} tail={live} />}
    </div>
  );
}

/* a running command's output stays at its newest line until the reader
   scrolls up in it */
function LiveOutput({ subject, tail }: { subject: string; tail: string }) {
  const box = useRef<HTMLDivElement>(null);
  useFollowNewest(box, { rows: tail.length, paused: false, enabled: true });
  return (
    <div ref={box} className="grid grid-cols-[minmax(0,1fr)] gap-1">
      <span className="font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
        live output
      </span>
      <ScrollArea
        orientation="both"
        data-testid={`tool-live-output-${subject}`}
        className="max-h-40 rounded-md bg-surface [&_[data-slot=scroll-area-viewport]]:max-h-[inherit]"
      >
        <pre className="w-max min-w-full px-3 py-2 font-mono text-[12px] leading-[18px]">
          {tail}
        </pre>
      </ScrollArea>
    </div>
  );
}

function RevealFile({ path }: { path: string }) {
  const node = useSelectedNode();
  const homeDid = useHomeDid();
  const label = revealInFolderLabel();
  const home = useHomeFolder();
  if (!label || !node || !isWorkingNode(node, homeDid) || !isAbsolutePath(path))
    return null;
  return (
    <Button
      variant="outline"
      size="xs"
      className="justify-self-start font-sans"
      onClick={() =>
        void revealInFolder(path).catch((error: unknown) =>
          toast(
            `Couldn't find ${shortPath(path, { home })} on this computer: ${String(error)}`,
          ),
        )
      }
    >
      {label}
    </Button>
  );
}
