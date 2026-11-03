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
import { cancelCauseText, sentence, stopSourceText } from "../../lib/reasonText";
import { Disclosure } from "../components/Disclosure";
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

/** Whether both of two outputs are there, so each needs its label to be
    told apart; a lone output reads as itself, like a terminal's. */
const both = (a?: string | null, b?: string | null) =>
  Boolean(a?.trim()) && Boolean(b?.trim());

function Payload({
  label,
  value,
  counted = false,
  tone,
}: {
  /** only where another output beside it needs telling apart */
  label?: string | null;
  value?: string | null;
  counted?: boolean;
  /** error output, drawn in the destructive tone */
  tone?: "error";
}) {
  if (!value?.trim()) return null;
  const text = pretty(value);
  const lines = counted ? lineCount(text) : 0;
  return (
    <div className="group/payload relative grid grid-cols-[minmax(0,1fr)] gap-1">
      {label ? (
        <div className="flex items-center justify-between">
          <span
            className={cn(
              "font-mono text-[10px] tracking-wide uppercase",
              tone === "error" ? "text-destructive" : "text-muted-foreground",
            )}
          >
            {label}
            {lines > 1 && (
              <span className="normal-case"> · {lines.toLocaleString()} lines</span>
            )}
          </span>
          <CopyButton getText={() => text} />
        </div>
      ) : (
        <CopyButton
          getText={() => text}
          className="absolute top-1 right-1 z-10 opacity-0 group-hover/payload:opacity-100 focus-visible:opacity-100"
        />
      )}
      <ScrollArea
        orientation="both"
        className={cn(
          "max-h-72 rounded-md [&_[data-slot=scroll-area-viewport]]:max-h-[inherit]",
          tone === "error"
            ? "border border-destructive/30 bg-destructive/5"
            : "bg-surface",
        )}
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
        <div className="grid gap-1.5">
          <p className="font-sans text-sm">{sentence(denial.reasonLine)}</p>
          <Disclosure summary="Details" summaryClassName="font-sans text-xs">
            <div className="mt-2 grid gap-2">
              <Meta
                items={[
                  `rule · ${denial.ruleId}`,
                  denial.deniedCommand && `command · ${denial.deniedCommand}`,
                  denial.deniedSubcommand && `subcommand · ${denial.deniedSubcommand}`,
                  denial.deniedArgument && `argument · ${denial.deniedArgument}`,
                ]}
              />
              <Payload value={denial.diagnostic} />
            </div>
          </Disclosure>
        </div>
      )}
      {tool.cancelCause && (
        <div className="grid gap-1.5">
          <p className="font-sans text-sm">
            {cancelCauseText(tool.cancelCause.cause)}
            {tool.cancelCause.at &&
              !Number.isNaN(Date.parse(tool.cancelCause.at)) &&
              ` at ${new Date(tool.cancelCause.at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}`}
            .
          </p>
          <Disclosure summary="Details" summaryClassName="font-sans text-xs">
            <div className="mt-2">
              <Meta
                items={[
                  `stopped by · ${stopSourceText(tool.cancelCause.source)}`,
                  ...tool.cancelCause.evidence,
                ]}
              />
            </div>
          </Disclosure>
        </div>
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
          <Payload value={p.stdout} counted />
          <Payload
            label={p.stdout?.trim() ? "Errors" : null}
            value={p.stderr}
            tone="error"
            counted
          />
          <Payload
            label={p.stdout?.trim() || p.stderr?.trim() ? "Output" : null}
            value={p.fallbackOutput}
            counted
          />
        </>
      )}
      {p.kind === "fileRead" && (
        <>
          <Payload
            label={both(p.body, p.fallbackOutput) ? "Contents" : null}
            value={p.body}
          />
          <Payload
            label={both(p.body, p.fallbackOutput) ? "Output" : null}
            value={p.fallbackOutput}
          />
        </>
      )}
      {p.kind === "fileEdit" && (
        <>
          {p.diff.length > 0 && <DiffBody diff={p.diff} path={p.path ?? "file"} />}
          {(p.revealPath || p.path) && tool.statusKind === "success" && (
            <RevealFile path={p.revealPath || p.path!} />
          )}
          <Payload value={p.fallbackOutput} />
        </>
      )}
      {p.kind === "agent" && (
        <>
          <Payload
            label={
              both(p.description, p.output)
                ? p.action === "start"
                  ? "Assignment"
                  : "Message"
                : null
            }
            value={p.description}
          />
          {p.sessionId && <Meta items={[`session · ${p.sessionId}`]} />}
          <Payload
            label={both(p.description, p.output) ? "Result" : null}
            value={p.output}
          />
        </>
      )}
      {p.kind === "process" && (
        <>
          <Payload
            label={both(p.description, p.output) ? "Arguments" : null}
            value={p.description}
          />
          <Payload
            label={both(p.description, p.output) ? "Result" : null}
            value={p.output}
          />
        </>
      )}
      {p.kind === "mcp" && (
        <>
          <Payload
            label={both(p.arguments, p.output) ? "Arguments" : null}
            value={p.arguments}
          />
          <Payload
            label={both(p.arguments, p.output) ? "Result" : null}
            value={p.output}
          />
        </>
      )}
      {p.kind === "generic" && (
        <>
          <Payload label={both(p.input, p.output) ? "Input" : null} value={p.input} />
          <Payload label={both(p.input, p.output) ? "Result" : null} value={p.output} />
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
