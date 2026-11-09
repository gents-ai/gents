import { SquareStack } from "lucide-react";
import { PanelEmpty } from "./PanelEmpty";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useShallow } from "zustand/react/shallow";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import type { SurfaceContext } from "@/app/surfaces";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { href } from "@/lib/router";
import { nodeOfSession } from "@/lib/nodes";
import { cn } from "@gents/ui/lib/utils";
import { stateOf } from "@/lib/live";
import { useFinishedSince } from "@/lib/seen";
import { NodeAgentStack } from "./NodeBehaviorStack";
import { SessionStatus } from "./SessionStatus";
import { StartedCount } from "./StartedCount";
import { useNow } from "@/lib/clock";
import { duration } from "@/lib/duration";
import { sessionViews } from "@/app/sessionViews";
import { sessionKeyOf } from "../../hooks/fleetStore";
import { startedUnder, useFleet, workersOf, workersOfId } from "../hooks/useFleet";

/* newest started first; a row keeps its place as its state changes */
const ordered = (workers: readonly SessionSummary[]) =>
  [...workers].sort(
    (a, b) => Date.parse(b.createdAt ?? "") - Date.parse(a.createdAt ?? "") || 0,
  );

/* how long it has worked: counting while it runs, to its last activity once
   it stops */
function workedMs(s: SessionSummary, now: number): number | null {
  const started = s.createdAt ? Date.parse(s.createdAt) : null;
  if (!started) return null;
  const state = stateOf(s.turnState);
  if (state === "working" || state === "waiting")
    return now > started ? now - started : null;
  const last = s.updatedAt ? Date.parse(s.updatedAt) : null;
  return last && last > started ? last - started : null;
}

/* what came back from the session, under its title; a reply that arrives
   while its row is up grows in, one there when the row opens is in place */
function ReplyLine({
  text,
  enter,
  onCut,
}: {
  text: string;
  enter: boolean;
  onCut: (cut: boolean) => void;
}) {
  const [entering] = useState(enter);
  const line = useRef<HTMLSpanElement>(null);
  useLayoutEffect(() => {
    const el = line.current;
    if (!el) return;
    const measure = () => onCut(el.scrollWidth > el.clientWidth);
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [text, onCut]);
  return (
    <div
      data-entering={entering || undefined}
      className="grid grid-rows-[1fr] motion-safe:data-entering:animate-[worker-reply-enter_200ms_ease-out]"
    >
      <p
        data-testid="worker-reply"
        className="flex min-h-0 min-w-0 items-baseline gap-1.5 text-xs text-foreground/75"
      >
        <span aria-hidden className="shrink-0 text-muted-foreground">
          ⎿
        </span>
        <span ref={line} className="min-w-0 truncate">
          {text}
        </span>
      </p>
    </div>
  );
}

type Look = { key: string; turnState: string | null | undefined; fresh: boolean };

/* the mark crossfades from the one it last showed while its row is up */
function EasedStatus({
  turnState,
  fresh,
  className,
}: {
  turnState: string | null | undefined;
  fresh: boolean;
  className?: string;
}) {
  const key = `${stateOf(turnState)}:${fresh}`;
  const [shown, setShown] = useState<{ now: Look; was: Look | null }>({
    now: { key, turnState, fresh },
    was: null,
  });
  if (shown.now.key !== key)
    setShown({ now: { key, turnState, fresh }, was: shown.now });
  const was = shown.was;
  return (
    <span className={cn("grid *:col-start-1 *:row-start-1", className)}>
      {was && (
        <span
          key={was.key}
          aria-hidden
          onAnimationEnd={(e) => {
            if (e.target === e.currentTarget) setShown((at) => ({ ...at, was: null }));
          }}
          className="animate-out fade-out-0 duration-150 ease-out fill-mode-forwards motion-reduce:hidden"
        >
          <SessionStatus turnState={was.turnState} fresh={was.fresh} />
        </span>
      )}
      <span
        key={key}
        data-entering={was ? true : undefined}
        className="motion-safe:data-entering:animate-in motion-safe:data-entering:fade-in-0 motion-safe:data-entering:duration-150 motion-safe:data-entering:ease-out"
      >
        <SessionStatus turnState={turnState} fresh={fresh} />
      </span>
    </span>
  );
}

type Opened = { has: (key: string) => boolean; toggle: (key: string) => void };

function WorkerRow({
  worker,
  opened,
  above,
}: {
  worker: SessionSummary;
  opened: Opened;
  /* the sessions this row is nested under, so a cycle cannot recurse */
  above: ReadonlySet<string>;
}) {
  const now = useNow(["working", "waiting"].includes(stateOf(worker.turnState)));
  const key = sessionKeyOf(worker);
  const children = useFleet((s) => workersOf(s, worker));
  const shown = useMemo(
    () => ordered(children.filter((c) => !above.has(sessionKeyOf(c)))),
    [children, above],
  );
  const open = opened.has(key) && shown.length > 0;
  const path = useMemo(() => new Set(above).add(key), [above, key]);
  const counts = useFleet(useShallow((s) => startedUnder(s, worker, path)));
  const took = workedMs(worker, now);
  const [cut, setCut] = useState(false);
  /* only what changes while the row is up eases in */
  const mounted = useRef(false);
  useEffect(() => {
    mounted.current = true;
  }, []);
  /* its own mark, not its starter's: here it is a row of its own */
  const fresh = useFinishedSince(worker);
  /* while it works the reply is partial, or an earlier turn's; the link
     covers the row, so it carries the reply's full text when it is cut */
  const state = stateOf(worker.turnState);
  const reply =
    state !== "working" && state !== "waiting"
      ? worker.replyPreview?.trim()
      : undefined;
  return (
    <li>
      <div className="relative grid grid-cols-[auto_1fr_auto_auto] items-start gap-x-2.5 rounded-lg bg-accent/40 px-3 py-2 text-sm hover:bg-accent">
        <EasedStatus turnState={worker.turnState} fresh={fresh} className="mt-0.5" />
        <div className="min-w-0">
          <div className="flex items-baseline gap-2">
            <a
              href={href({ name: "session", sessionId: worker.sessionId })}
              title={reply && cut ? reply : undefined}
              className="min-w-0 flex-1 truncate outline-none after:absolute after:inset-0 after:rounded-lg focus-visible:after:ring-2 focus-visible:after:ring-ring"
            >
              {worker.title ?? "Untitled"}
            </a>
            {took !== null && (
              <span
                data-testid="worker-took"
                className="shrink-0 text-xs text-muted-foreground tabular-nums"
              >
                {duration(took)}
              </span>
            )}
          </div>
          {reply && <ReplyLine text={reply} enter={mounted.current} onCut={setCut} />}
        </div>
        <span className="flex justify-end">
          <StartedCount
            session={worker}
            {...counts}
            open={open}
            onToggle={() => opened.toggle(key)}
          />
        </span>
        <span className="relative z-10">
          <NodeAgentStack
            nodeDid={nodeOfSession(worker)}
            agentDid={worker.agentDid}
            size="sm"
          />
        </span>
      </div>
      {open && (
        <ul className="mt-1.5 ml-3 space-y-1.5 border-l border-border/50 pl-3">
          {shown.map((w) => (
            <WorkerRow key={sessionKeyOf(w)} worker={w} opened={opened} above={path} />
          ))}
        </ul>
      )}
    </li>
  );
}

export function WorkersSurface({ sessionId }: SurfaceContext) {
  const workers = useFleet((s) => workersOfId(s, sessionId));
  const shown = useMemo(() => ordered(workers), [workers]);
  /* which rows show their started sessions, kept for the next visit; the
     dock can stay up while another session opens */
  const [held, setHeld] = useState(() => ({
    session: sessionId,
    open: new Set(sessionViews.openedOf(sessionId)) as ReadonlySet<string>,
  }));
  if (held.session !== sessionId)
    setHeld({ session: sessionId, open: new Set(sessionViews.openedOf(sessionId)) });
  const open = held.open;
  const toggle = useCallback(
    (key: string) =>
      setHeld((was) => {
        const next = new Set(was.open);
        if (!next.delete(key)) next.add(key);
        if (was.session) sessionViews.setOpened(was.session, [...next]);
        return { session: was.session, open: next };
      }),
    [],
  );
  const opened = useMemo<Opened>(
    () => ({ has: (k) => open.has(k), toggle }),
    [open, toggle],
  );
  const lead = useFleet((s) => (sessionId ? s.bySessionId[sessionId] : undefined));
  const top = useMemo(() => new Set(lead ? [sessionKeyOf(lead)] : []), [lead]);
  if (shown.length === 0)
    return (
      <PanelEmpty icon={SquareStack} title="No started sessions">
        Sessions this one starts to hand out work show here.
      </PanelEmpty>
    );
  return (
    <ScrollArea className="h-full">
      <div className="px-3 py-2">
        <ul className="space-y-1.5">
          {shown.map((w) => (
            <WorkerRow key={sessionKeyOf(w)} worker={w} opened={opened} above={top} />
          ))}
        </ul>
      </div>
    </ScrollArea>
  );
}
