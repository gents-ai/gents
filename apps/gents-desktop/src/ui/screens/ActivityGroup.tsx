import { createContext, useContext, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown } from "lucide-react";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { cn } from "@gents/ui/lib/utils";
import { ToolStep, ToolSteps } from "@gents/ui/conversation";
import { holdRow, useFollowNewest, useScrollEdges } from "@/lib/scroll";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import { KEEP_DRAWN } from "@/lib/transcriptWindow";

import { BackgroundToolButton } from "./BackgroundToolButton";
import { ToolIcon } from "./tool-icon";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { Spinner } from "@gents/ui/components/spinner";
import { agentName } from "./behavior";
import { AgentInitials } from "./parts";
import { Markdown } from "./Markdown";
import { ToolBody } from "./tool-views";
import { gatherWorkers, workerStory, type GatheredWorker } from "./worker-gathering";
import { WorkerStep, isWorkerStep } from "./WorkerStep";
import { type Workers } from "./workers";
import { diffTally, stepStatus, toolSummary } from "./tool-summary";
import {
  groupFailures,
  groupLabel,
  groupTools,
  liveGroupLabel,
  type GroupMember,
  type TranscriptEntry,
} from "./transcript-groups";
import { useSelectedNode } from "@/hooks/useClient";

function Step({
  tool,
  workers,
  note,
}: {
  tool: RenderedToolCallView;
  workers: Workers;
  note?: string | null;
}) {
  if (isWorkerStep(tool)) return <WorkerStep tool={tool} workers={workers} />;
  const summary = toolSummary(tool);
  const open = tool.statusKind !== "running";
  const p = tool.presentation;
  const tally = p.kind === "fileEdit" ? diffTally(p.diff) : null;
  const status = stepStatus(tool);
  const outcome =
    status === "failed" || status === "stopped" ? summary.secondary : null;
  return (
    <ToolStep
      label={`${summary.kind} ${summary.primary}`.trim()}
      icon={<ToolIcon tool={tool} />}
      meta={
        <>
          {tally ?? outcome ?? undefined}
          <BackgroundToolButton tool={tool} />
        </>
      }
      status={status}
      stepKey={tool.itemKey}
    >
      {open || note ? (
        <>
          {note && <p className="pb-2 text-xs text-muted-foreground">{note}</p>}
          {open ? <ToolBody tool={tool} /> : null}
        </>
      ) : undefined}
    </ToolStep>
  );
}

type GroupState = {
  folded: Record<string, boolean>;
  openStep: Record<string, string | null>;
  setFolded: (groupKey: string, folded: boolean) => void;
  setOpenStep: (groupKey: string, stepKey: string | null) => void;
};
export const GroupStateContext = createContext<GroupState | null>(null);

export function useGroupState(sessionKey: string | null): GroupState {
  const [folded, setFoldedMap] = useState<Record<string, boolean>>({});
  const [openStep, setOpenStepMap] = useState<Record<string, string | null>>({});
  const [session, setSession] = useState(sessionKey);
  if (session !== sessionKey) {
    setSession(sessionKey);
    setFoldedMap({});
    setOpenStepMap({});
  }
  return useMemo(
    () => ({
      folded,
      openStep,
      setFolded: (key, value) => setFoldedMap((m) => ({ ...m, [key]: value })),
      setOpenStep: (key, value) => setOpenStepMap((m) => ({ ...m, [key]: value })),
    }),
    [folded, openStep],
  );
}

function WorkerRunStep({
  tools,
  workers,
}: {
  tools: RenderedToolCallView[];
  workers: Workers;
}) {
  const deployment = useSelectedNode();
  const first = tools[0]!;
  const p = first.presentation;
  const reached = workers.byToolCall(first);
  const name =
    reached?.summary?.title ?? (p.kind === "agent" ? p.name : null) ?? "a session";
  const last = tools[tools.length - 1]!;
  const failed = tools.some(
    (t) => t.statusKind === "error" || t.statusKind === "failed",
  );
  const agentId = reached?.summary?.agentId ?? null;
  return (
    <ToolStep
      label={name}
      icon={
        agentId ? (
          <AgentInitials
            name={agentName(agentId, deployment)}
            className="size-4 text-[8px]"
          />
        ) : (
          <ToolIcon tool={first} />
        )
      }
      detail={workerStory(tools)}
      status={last.statusKind === "running" ? "running" : failed ? "failed" : "done"}
    >
      <ToolSteps className="-mx-2">
        {tools.map((tool) => (
          <WorkerStep key={tool.itemKey} tool={tool} workers={workers} />
        ))}
      </ToolSteps>
    </ToolStep>
  );
}

type PlacedMember = GroupMember | ({ kind: "worker" } & GatheredWorker);

function placeWorkers(members: GroupMember[]): PlacedMember[] {
  const gathered = gatherWorkers(groupTools(members));
  const placed = new Set<string>();
  const out: PlacedMember[] = [];
  for (const m of members) {
    const worker = m.kind === "tool" ? gathered.get(m.key) : undefined;
    if (!worker) {
      out.push(m);
      continue;
    }
    if (placed.has(worker.key)) continue;
    placed.add(worker.key);
    out.push({ kind: "worker", ...worker });
  }
  return out;
}

export function ActivityGroup({
  entry,
  workers,
}: {
  entry: Extract<TranscriptEntry, { kind: "group" }>;
  workers: Workers;
}) {
  const state = useContext(GroupStateContext)!;
  const single = entry.members.length === 1 && entry.members[0]!.kind === "tool";
  const chosen = state.folded[entry.key];
  const openStep = state.openStep[entry.key] ?? null;

  const failed = groupFailures(entry.members) > 0;
  const folded = !single && (chosen ?? !(openStep || failed));

  const live = (!entry.settled && liveGroupLabel(entry.members)) || null;
  const [animated, setAnimated] = useState(false);
  const root = useRef<HTMLDivElement>(null);

  const box = useRef<HTMLDivElement>(null);
  const capped = !single;
  const more = useScrollEdges(box);
  useFollowNewest(box, {
    rows: entry.members.length,
    paused: openStep !== null,
    enabled: capped,
  });

  return (
    <div ref={root} data-anchor-key={entry.key}>
      {!single && (
        <button
          type="button"
          onClick={() => {
            holdRow(root.current);
            setAnimated(true);
            state.setFolded(entry.key, !folded);
          }}
          aria-expanded={!folded}
          aria-busy={live ? true : undefined}
          data-failed={failed || undefined}
          className={cn(
            "flex w-full cursor-pointer items-center gap-3 px-2 py-1 text-left text-sm hover:text-foreground",
            live ? "font-medium text-foreground" : "text-muted-foreground",
          )}
        >
          <span className="grid size-4 shrink-0 place-items-center">
            <ChevronDown
              className={cn(
                "size-3.5 transition-transform duration-150 motion-reduce:transition-none",
                folded && "-rotate-90",
              )}
            />
          </span>
          <span className="flex min-w-0 items-center gap-1.5">
            {live && <Spinner className="size-3.5 shrink-0 motion-reduce:hidden" />}
            <span className="min-w-0 truncate">
              {live ?? groupLabel(entry.members)}
            </span>
          </span>
        </button>
      )}
      <div
        inert={folded || undefined}
        className={cn(
          "grid ease-out motion-reduce:transition-none",
          animated && "transition-[grid-template-rows] duration-200",
          folded ? "grid-rows-[0fr]" : "grid-rows-[1fr]",
        )}
      >
        <div ref={box} className="relative min-h-0 overflow-hidden">
          <ScrollArea
            className={cn(
              capped && "max-h-80",
              "[&>[data-slot=scroll-area-viewport]]:max-h-[inherit]",
            )}
          >
            <ToolSteps
              openId={openStep}
              onOpenIdChange={(id) => state.setOpenStep(entry.key, id)}
            >
              {placeWorkers(entry.members).map((m) =>
                m.kind === "tool" ? (
                  <Step key={m.key} tool={m.tool} workers={workers} />
                ) : m.kind === "worker" ? (
                  <WorkerRunStep key={m.key} tools={m.tools} workers={workers} />
                ) : (
                  <div key={m.key} className="py-1">
                    <Reasoning text={m.text} />
                  </div>
                ),
              )}
            </ToolSteps>
          </ScrollArea>
          <div
            aria-hidden
            className={cn(
              "pointer-events-none absolute inset-x-0 top-0 h-6 bg-gradient-to-b from-background to-transparent transition-opacity duration-200",
              capped && more.above ? "opacity-100" : "opacity-0",
            )}
          />
          <div
            aria-hidden
            className={cn(
              "pointer-events-none absolute inset-x-0 bottom-0 h-8 bg-gradient-to-t from-background to-transparent transition-opacity duration-200",
              capped && more.below ? "opacity-100" : "opacity-0",
            )}
          />
        </div>
      </div>
    </div>
  );
}

export function Reasoning({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const [all, setAll] = useState(false);
  const [clipped, setClipped] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const words = text.trim().split(/\s+/).length;
  useEffect(() => {
    const el = body.current;
    if (!open || !el) return;
    const measure = () => setClipped(el.scrollHeight > el.clientHeight + 1);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [open, all, text]);
  const fold = (next: () => void) => {
    holdRow(root.current);
    next();
  };
  return (
    <Collapsible
      ref={root}
      open={open}
      {...(open ? { [KEEP_DRAWN]: "" } : {})}
      onOpenChange={(next) => (next ? setOpen(true) : fold(() => setOpen(false)))}
      className="mb-5"
    >
      <CollapsibleTrigger className="-mx-2 -my-1 flex cursor-pointer items-center gap-1.5 rounded-lg px-2 py-1 text-xs text-muted-foreground transition-colors hover:bg-accent/30 hover:text-foreground">
        <ChevronDown
          className={cn("size-3.5 transition-transform", open && "rotate-180")}
        />
        Thinking
        <span className="tabular-nums opacity-70">
          · {words.toLocaleString()} words
        </span>
      </CollapsibleTrigger>
      <CollapsibleContent className="mt-1 border-l border-border pl-3">
        <div
          ref={body}
          className={cn(
            "relative text-muted-foreground",
            !all && "max-h-80 overflow-hidden",
          )}
        >
          <Markdown>{text}</Markdown>
          {!all && clipped && (
            <div className="pointer-events-none absolute inset-x-0 bottom-0 h-16 bg-gradient-to-t from-background to-transparent" />
          )}
        </div>
        {(all || clipped) && (
          <div
            className={cn(
              "mt-1 flex w-fit items-center gap-1 rounded-lg p-0.5",
              all &&
                "sticky bottom-[calc(var(--composer-h,0px)+0.75rem)] z-10 border border-border bg-raised/95 shadow-sm backdrop-blur",
            )}
          >
            {all ? (
              <Button
                variant="quiet"
                size="xs"
                onClick={() => fold(() => setAll(false))}
              >
                Show less
              </Button>
            ) : (
              <Button variant="quiet" size="xs" onClick={() => setAll(true)}>
                Show all {words.toLocaleString()} words
              </Button>
            )}
            <Button
              variant="quiet"
              size="xs"
              onClick={() => fold(() => setOpen(false))}
            >
              Close
            </Button>
          </div>
        )}
      </CollapsibleContent>
    </Collapsible>
  );
}
