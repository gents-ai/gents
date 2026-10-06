/* A run of tool steps folded into one group, with the worker sessions its
   calls started, and the reasoning shown beside a step or a reply. */
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

import { ToolIcon } from "./tool-icon";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { Spinner } from "@gents/ui/components/spinner";
import { behaviorName } from "./behavior";
import { BehaviorAvatar } from "./parts";
import { Markdown } from "./Markdown";
import { ToolBody } from "./tool-views";
import { gatherWorkers, workerStory, type GatheredWorker } from "./worker-gathering";
import { WorkerStep, isWorkerStep } from "./WorkerStep";
import { type Workers } from "./workers";
import { diffTally, stepStatus, toolSummary } from "./tool-summary";
import {
  groupLabel,
  groupTools,
  liveGroupLabel,
  type GroupMember,
  type TranscriptEntry,
} from "./transcript-groups";
import { useSelectedNode } from "@/hooks/useClient";

/* one tool call as a row, wherever it sits: loose in the group, or among
   the calls a run folded together */
function Step({
  tool,
  workers,
  note,
}: {
  tool: RenderedToolCallView;
  workers: Workers;
  /* what the agent said before making this call. Inside a run it is not
     shown in the rows — it mostly restates the row beneath it — but it is
     the agent's own account and is kept where the call itself is opened. */
  note?: string | null;
}) {
  if (isWorkerStep(tool)) return <WorkerStep tool={tool} workers={workers} />;
  const summary = toolSummary(tool);
  const open = tool.statusKind !== "running";
  /* how big the edit was, on the row rather than inside it: the size of a
     change is most of what a person wants from it, and asking them to open
     the diff to find out makes them open every one */
  const p = tool.presentation;
  const tally = p.kind === "fileEdit" ? diffTally(p.diff) : null;
  const status = stepStatus(tool);
  /* how it ended, where the tally sits: `exit 1`, `timed out`. Only when
     it did not simply work — a row that says "exit 0" after every command
     is the noise this column has been kept clear of. */
  const outcome =
    status === "failed" || status === "stopped" ? summary.secondary : null;
  return (
    <ToolStep
      label={`${summary.kind} ${summary.primary}`.trim()}
      icon={<ToolIcon tool={tool} />}
      meta={tally ?? outcome ?? undefined}
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
    reached?.summary?.title ?? (p.kind === "subagent" ? p.name : null) ?? "a session";
  const last = tools[tools.length - 1]!;
  const failed = tools.some(
    (t) => t.statusKind === "error" || t.statusKind === "failed",
  );
  /* the row is about one agent, so it wears that agent's mark: the same
     avatar the session list and the parent's turns use. A worker with
     no session summary has no behavior to wear, and falls back to the
     kind's glyph. */
  const behaviorId = reached?.summary?.behaviorId ?? null;
  return (
    <ToolStep
      label={name}
      icon={
        behaviorId ? (
          <BehaviorAvatar
            name={behaviorName(behaviorId, deployment)}
            className="size-4 text-[8px]"
          />
        ) : (
          <ToolIcon tool={first} />
        )
      }
      detail={workerStory(tools)}
      status={last.statusKind === "running" ? "running" : failed ? "pending" : "done"}
    >
      <ToolSteps className="-mx-2">
        {tools.map((tool) => (
          <WorkerStep key={tool.itemKey} tool={tool} workers={workers} />
        ))}
      </ToolSteps>
    </ToolStep>
  );
}

/* A worker's scattered steps as one row inside its group, at its first step. */
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

/* A stretch of consecutive calls, placed once (transcript-groups.ts) and
   only ever added to at its end.

   It is the same element from its first call: a lone call shows as its
   row, and when a second arrives that row gives way to a header of the
   same height, without rebuilding the rows. The group stays folded to
   that header until the reader opens it; while a call runs, the header
   says what it is doing.

   Opened, its rows sit in a box about eight rows tall that follows the
   newest call — unless the reader has scrolled inside it — so a long burst
   grows inside the box instead of pushing the transcript around. A step
   opened in it opens inside the box too: the group keeps its size, and the
   box stops following new calls while the step is open, so what the
   reader opened is not scrolled away from them. */
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
  /* Folded to its header until the reader opens it — live or finished.
     Its header says what it is doing while a call runs, so a live group
     shows its work in one line that never changes height; a lone call is
     its own row, and becomes the header (the same height) when a second
     arrives. A step the reader opened before the group formed keeps the
     group open, so what they opened does not disappear under a fold. */
  const folded = !single && (chosen ?? !openStep);
  /* what it is doing right now, while a call in it runs */
  const live = (!entry.settled && liveGroupLabel(entry.members)) || null;
  /* only a fold the reader asks for is animated */
  const [animated, setAnimated] = useState(false);
  const root = useRef<HTMLDivElement>(null);

  /* the capped box scrolls in the kit's ScrollArea, like the transcript
     and the diffs: a plain overflow box drew WebKit's wide scrollbar */
  const box = useRef<HTMLDivElement>(null);
  const capped = !single;
  /* whether rows are hidden past either edge of the box: a fade says so,
     the way the transcript's own edge does. A live box follows the newest
     call, so what it hides is usually above. */
  const more = useScrollEdges(box);
  /* an open step grows the box without a scroll, so following waits for it */
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
            /* the transcript holds the header still while the rows move */
            holdRow(root.current);
            setAnimated(true);
            state.setFolded(entry.key, !folded);
          }}
          aria-expanded={!folded}
          aria-busy={live ? true : undefined}
          className={cn(
            "flex w-full cursor-pointer items-center gap-3 px-2 py-1 text-left text-sm hover:text-foreground",
            /* a group at work reads like a step at work: full ink and a
               little weight */
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
          {/* the caret stays the caret — it is what says this opens — and a
              group at work shows the loader beside its label, the way the
              Thinking line does */}
          <span className="flex min-w-0 items-center gap-1.5">
            {live && <Spinner className="size-3.5 shrink-0 motion-reduce:hidden" />}
            <span className="min-w-0 truncate">
              {live ?? groupLabel(entry.members)}
            </span>
          </span>
        </button>
      )}
      {/* Folding animates the row track from 1fr to 0fr: the height is the
          content's own, with nothing to measure, and the rows stay mounted
          so nothing about them is rebuilt. A folded group is inert, so
          focus cannot land in rows no one can see. */}
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
              /* a scroll that starts in the box stays in the box: reaching its end
                 does not carry on into the transcript (overscroll-behavior, which
                 the ScrollArea's native viewport honours). The box's own viewport
                 only, a direct child: a code block inside an opened step
                 keeps its own scroll, and its end carries on into the box */
              "[&>[data-slot=scroll-area-viewport]]:max-h-[inherit] [&>[data-slot=scroll-area-viewport]]:overscroll-contain",
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
          {/* rows hidden past an edge of the box: the same fade the transcript's
              own edge uses */}
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

/* The model's reasoning, folded under the answer the way the desktop does.
   It runs to thousands of words, so opening it shows a screenful and says
   how much more there is. A tool's contents scroll inside their frame,
   because they are reference to dip into; reasoning is prose read from the
   top, and a scroller inside a transcript traps the wheel and stops a
   reader scanning past it. It stays quieter than the answer it explains. */
export function Reasoning({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const [all, setAll] = useState(false);
  /* whether there is anything behind the fade: a short reasoning needs no
     way to expand it, and offering one says there is more to read */
  const [clipped, setClipped] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const words = text.trim().split(/\s+/).length;
  useEffect(() => {
    const el = body.current;
    if (!open || !el) return;
    const measure = () => setClipped(el.scrollHeight > el.clientHeight + 1);
    measure();
    /* prose reflows as fonts land and the column resizes */
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [open, all, text]);
  /* folding thousands of words away moves everything under them; the
     transcript holds the block where the reader left it */
  const fold = (next: () => void) => {
    holdRow(root.current);
    next();
  };
  return (
    <Collapsible
      ref={root}
      open={open}
      onOpenChange={(next) => (next ? setOpen(true) : fold(() => setOpen(false)))}
      /* the answer is the thing a person came for, so it does not begin
         directly under the working-out that led to it */
      className="mb-5"
    >
      {/* the controls at the foot are the way out, so the trigger stays put:
          two sticky things for one block meet in the middle as soon as the
          block is short, and then neither is where it was reached for */}
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
        {/* the way out sits where the reading ends, not back up at the top,
            and it is there whether or not the rest was ever unfolded; a
            reasoning that fits on a screen needs neither */}
        {(all || clipped) && (
          /* at the foot of the view while the block is in it, clear of the
             composer, so a long reasoning can be left without reading to
             the end of it */
          <div
            className={cn(
              "mt-1 flex w-fit items-center gap-1 rounded-lg p-0.5",
              /* only a block taller than the view has anywhere to stick:
                 clipped, it is a screenful and its foot is already in
                 reach, and sticky inside a short box just parks the
                 controls at its end, which may be under the composer */
              /* the composer floats over the foot of the scroller, and
                 padding the transcript does not move a sticky element:
                 it pins against the scrollport, so the offset is still
                 the room the composer leaves */
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
