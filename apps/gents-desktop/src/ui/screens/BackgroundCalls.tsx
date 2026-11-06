import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { SurfaceContext } from "@/app/surfaces";
import {
  ArrowLeft,
  Ban,
  CircleCheck,
  CircleX,
  MessageSquareText,
  Terminal,
  TriangleAlert,
} from "lucide-react";
import { PanelEmpty } from "./PanelEmpty";
import { DayMarker, dayOpenings } from "./TranscriptMarker";
import { toast } from "sonner";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { useApp } from "@/app/AppContext";
import { openAt } from "@/app/messagePlaces";
import { useSelectedSessionValue, useSessionFacts } from "@/hooks/useSelectedSession";
import { toastFailure } from "@/lib/failure";
import {
  isActive,
  nextWrite,
  outputOf,
  stateOf,
  useBackgroundCalls,
  type BackgroundCall,
  type Tone,
  useAskedLog,
  backgroundPlace,
  rememberBackgroundPlace,
  rowStateOf,
  transcriptPlaceOf,
} from "./background";
import { Hint } from "./Hint";
import { useNow } from "@/lib/clock";
import { openTerminal, type LogTerminal } from "./terminal";
import { useMinute } from "./time";
import { StopButton } from "./WorkerActions";
import { SEARCH_FIELD } from "@/lib/textAssist";

function ToneIcon({ tone, className }: { tone: Tone; className?: string }) {
  return (
    <span className={cn("grid size-4 shrink-0 place-items-center", className)}>
      {tone === "running" ? (
        <Spinner className="text-foreground" />
      ) : tone === "stuck" || tone === "overdue" ? (
        <TriangleAlert
          className={cn(
            "size-3.5",
            tone === "stuck" ? "text-destructive" : "text-muted-foreground",
          )}
        />
      ) : tone === "failed" ? (
        <CircleX className="size-3.5 text-destructive" />
      ) : tone === "done" ? (
        <CircleCheck className="size-3.5 text-muted-foreground" />
      ) : (
        <Ban className="size-3.5 text-muted-foreground" />
      )}
    </span>
  );
}

const warns = (tone: Tone) => tone === "failed" || tone === "stuck";

const job = (call: BackgroundCall) =>
  Boolean(
    call.tool?.jobState ||
    call.tool?.jobHandle ||
    call.tool?.jobOrigin ||
    call.view?.job,
  );
const stoppable = (call: BackgroundCall) =>
  isActive(call) && Boolean(call.tool?.toolCallDocId);

function Stop({ call, sessionId }: { call: BackgroundCall; sessionId: string | null }) {
  const { actions } = useApp();
  const nodeDid = call.tool?.nodeDid;
  const toolCallDocId = call.tool?.toolCallDocId;
  const isJob = job(call);
  if (!nodeDid || !sessionId || !toolCallDocId || !stoppable(call)) return null;
  if (!isJob && call.view?.job !== false) return null;
  const stop = async () => {
    try {
      const result = isJob
        ? await actions.controlJob({ nodeDid, toolCallDocId, action: "stop" })
        : await actions.controlProcess({
            nodeDid,
            sessionId,
            toolCallDocId,
            toolCallId: call.toolCallId,
          });
      const tag = (typeof result === "string" ? result : (Object.keys(result)[0] ?? ""))
        .toLowerCase()
        .replace(/_/g, "");
      if (tag === "cancelled" || tag === "applied")
        toast.success(`Stopped ${call.runs}`);
      else if (tag === "queued") toast.info(`Stop requested for ${call.runs}.`);
      else if (tag === "alreadyterminal")
        toast.info(`${call.runs} has already finished.`);
      else if (tag === "unverified")
        toast.warning(`Could not confirm that ${call.runs} stopped.`);
      else if (tag === "lost")
        toast.warning(`The runtime no longer has ownership of ${call.runs}.`);
      else if (tag === "notfound")
        toast.warning(`The selected process is no longer available.`);
      else if (tag === "notbackground")
        toast.warning(`The selected call cannot be stopped as a background process.`);
      else toast.warning(`The result of stopping ${call.runs} is unavailable.`);
    } catch (error) {
      toastFailure("stop", error);
    }
  };
  return <StopButton name={call.runs} onStop={() => void stop()} />;
}

const SHOW = "Show in transcript";

/* opens the transcript at the step that started the call, when the
   session's rows place it */
function useShowInTranscript(call: BackgroundCall, sessionId: string | null) {
  const facts = useSessionFacts();
  const nodeDid = useSelectedSessionValue((s) =>
    s?.sessionId === sessionId ? (s.nodeDid ?? null) : null,
  );
  const place = facts ? transcriptPlaceOf(call, facts.tools, facts.toolMessages) : null;
  if (!sessionId || !place) return null;
  return () =>
    openAt({
      sessionId,
      nodeDid,
      sequence: place.sequence,
      itemKey: place.itemKey,
      open: true,
    });
}

export function BackgroundGroup({
  calls,
  sessionId,
  returned,
  onOpen,
}: {
  calls: readonly BackgroundCall[];
  sessionId: string | null;
  /* the call whose output the reader just left */
  returned: string | null;
  onOpen: (key: string) => void;
}) {
  const now = useMinute();
  const running = calls.filter(isActive);
  const finished = calls.filter((call) => !isActive(call));
  return (
    <>
      <CallGroup
        title="Running"
        calls={running}
        now={now}
        sessionId={sessionId}
        returned={returned}
        onOpen={onOpen}
      />
      <CallGroup
        title="Finished"
        calls={finished}
        now={now}
        sessionId={sessionId}
        returned={returned}
        onOpen={onOpen}
        quiet
        byDay
      />
    </>
  );
}

function CallRow({
  call,
  minute,
  sessionId,
  returned,
  onOpen,
  quiet,
  day,
}: {
  call: BackgroundCall;
  minute: number;
  sessionId: string | null;
  returned: boolean;
  onOpen: (key: string) => void;
  quiet: boolean;
  /** the day this row opens, when the list spans more than one */
  day?: number;
}) {
  const running = isActive(call);
  const second = useNow(running);
  const now = running && second ? second : minute;
  const state = stateOf(call, now);
  const line = rowStateOf(call, now);
  const show = useShowInTranscript(call, sessionId);
  return (
    <li>
      {day !== undefined && <DayMarker day={day} />}
      <div
        title={`${call.runs}\n${state.text}`}
        data-returned={returned || undefined}
        className={cn(
          "group/row relative grid grid-cols-[auto_minmax(0,1fr)_auto] items-start gap-x-2.5 rounded-lg px-2 py-2 text-sm transition-colors duration-500 hover:bg-accent motion-reduce:transition-none",
          returned && "bg-accent",
        )}
      >
        <ToneIcon tone={state.tone} className="mt-0.5" />
        <div className="min-w-0">
          <button
            type="button"
            onClick={() => onOpen(call.key)}
            className={cn(
              "block w-full truncate text-left font-mono text-xs leading-5 outline-none after:absolute after:inset-0 after:rounded-lg focus-visible:after:ring-2 focus-visible:after:ring-ring",
              quiet && "text-muted-foreground",
            )}
          >
            {call.runs}
          </button>
          <p
            data-testid="background-state"
            className="truncate text-xs text-muted-foreground tabular-nums"
          >
            <span className={cn(warns(state.tone) && "text-destructive")}>
              {line.state}
            </span>
            {line.more && ` · ${line.more}`}
          </p>
        </div>
        <span className="relative z-10 flex items-center gap-0.5 self-center">
          {show && (
            <Hint label={SHOW}>
              <Button
                variant="quiet"
                size="icon-xs"
                aria-label={SHOW}
                onClick={show}
                className="opacity-0 transition-opacity group-hover/row:opacity-100 group-has-focus-visible/row:opacity-100 focus-visible:opacity-100 motion-reduce:transition-none [@media(hover:none)]:opacity-100"
              >
                <MessageSquareText className="size-3.5" />
              </Button>
            </Hint>
          )}
          <Stop call={call} sessionId={sessionId} />
        </span>
      </div>
    </li>
  );
}

function CallGroup({
  title,
  calls,
  now,
  sessionId,
  returned,
  onOpen,
  quiet = false,
  byDay = false,
}: {
  title: string;
  calls: readonly BackgroundCall[];
  now: number;
  sessionId: string | null;
  returned: string | null;
  onOpen: (key: string) => void;
  quiet?: boolean;
  /** marks where each day starts, once the calls span more than one */
  byDay?: boolean;
}) {
  if (calls.length === 0) return null;
  /* every day the rows open is marked, today's too, since the rows run newest
     first; one day alone gets no marker */
  const opens = byDay
    ? dayOpenings(
        calls,
        (call) => call.key,
        (call) => call.tool?.startedAt,
        Number.NaN,
      )
    : new Map<string, number>();
  const days = opens.size > 1 ? opens : new Map<string, number>();
  return (
    <section aria-label={title} className="mb-3">
      <h3 className="px-1 pb-1 text-xs font-medium text-muted-foreground">{title}</h3>
      <ul className="space-y-0.5">
        {calls.map((call) => (
          <CallRow
            key={call.key}
            call={call}
            minute={now}
            sessionId={sessionId}
            returned={call.key === returned}
            onOpen={onOpen}
            quiet={quiet}
            day={days.get(call.key)}
          />
        ))}
      </ul>
    </section>
  );
}

/* the newest output seen for a call stays shown when a later read of the
   session carries none, as a spawned call's rows do once it ends */
function useKeptOutput(call: BackgroundCall): string {
  const output = outputOf(call.tool);
  const [kept, setKept] = useState({ key: call.key, text: output });
  if (output && (kept.key !== call.key || kept.text !== output))
    setKept({ key: call.key, text: output });
  return output || (kept.key === call.key ? kept.text : "");
}

export function BackgroundLog({
  call,
  sessionId,
  onBack,
}: {
  call: BackgroundCall;
  sessionId: string | null;
  onBack: () => void;
}) {
  const now = useMinute();
  const state = stateOf(call, now);
  const output = useKeptOutput(call);
  const show = useShowInTranscript(call, sessionId);
  const p = call.tool?.presentation;
  const cwd = p?.kind === "command" ? p.cwd : null;
  return (
    <div className="flex h-full min-h-0 flex-col gap-2 px-3 py-2">
      <div className="flex items-center gap-1">
        <Button
          variant="ghost"
          size="sm"
          className="-ml-1.5 h-7 gap-1 px-1.5"
          onClick={onBack}
        >
          <ArrowLeft className="size-3.5" />
          Back
        </Button>
        {show && (
          <Button variant="ghost" size="sm" className="h-7 gap-1 px-1.5" onClick={show}>
            <MessageSquareText className="size-3.5" />
            {SHOW}
          </Button>
        )}
        <span className="flex-1" />
        <Stop call={call} sessionId={sessionId} />
      </div>
      <div className="grid gap-1">
        <p
          data-testid="background-command"
          className="font-mono text-xs leading-5 break-all whitespace-pre-wrap"
        >
          {call.runs}
        </p>
        {cwd && <p className="truncate text-xs text-muted-foreground">in {cwd}</p>}
        <p
          className={cn(
            "flex items-center gap-1.5 text-xs",
            warns(state.tone) ? "text-destructive" : "text-muted-foreground",
          )}
        >
          <ToneIcon tone={state.tone} />
          {state.text}
        </p>
      </div>
      {!output && !isActive(call) ? (
        <p
          data-testid="background-no-output"
          className="m-auto max-w-64 text-center text-sm text-muted-foreground"
        >
          {/* the runtime keeps a spawned process's output, but its rows stop
              carrying it once the process ends */}
          {call.toolCallId.startsWith("spawned:")
            ? "The output isn't available after this process ended."
            : "It finished without printing anything."}
        </p>
      ) : (
        <Output key={call.key} text={output} sessionId={sessionId} callKey={call.key} />
      )}
    </div>
  );
}

const SGR = /\x1b\[[0-9;]*m/g;

function Output({
  text,
  sessionId,
  callKey,
}: {
  text: string;
  sessionId: string | null;
  callKey: string;
}) {
  const { actions } = useApp();
  const link = useRef(actions.openExternalUrl);
  link.current = actions.openExternalUrl;
  const host = useRef<HTMLDivElement>(null);
  const [term, setTerm] = useState<LogTerminal | null>(null);
  const [failed, setFailed] = useState(false);
  const [query, setQuery] = useState("");
  const shown = useRef("");
  useEffect(() => {
    const el = host.current;
    if (!el) return;
    let live = true;
    let made: LogTerminal | null = null;
    const place = backgroundPlace(sessionId);
    openTerminal(el, {
      onLink: (url) => void link.current(url),
      above: place.log === callKey ? place.above : 0,
    }).then(
      (t) => {
        if (!live) return t.dispose();
        made = t;
        shown.current = "";
        setTerm(t);
      },
      () => live && setFailed(true),
    );
    return () => {
      live = false;
      /* left without Back (another tab, the dock closed): keep the place */
      if (made && backgroundPlace(sessionId).log === callKey)
        rememberBackgroundPlace(sessionId, { log: callKey, above: made.linesAbove() });
      made?.dispose();
    };
  }, [sessionId, callKey]);
  useEffect(() => {
    if (!term) return;
    const step = nextWrite(shown.current, text);
    if (!step) return;
    term.write(step.data, step.reset);
    shown.current = text;
  }, [term, text]);
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-1.5">
      <div className="flex items-center gap-2">
        <span
          title="Only the last part of the output is shown."
          className="font-mono text-[10px] tracking-wide text-muted-foreground uppercase"
        >
          Output (last part)
        </span>
        <span className="flex-1" />
        {term && (
          <Input
            {...SEARCH_FIELD}
            type="search"
            value={query}
            aria-label="Find in output"
            placeholder="Find in output"
            className="h-7 w-36 text-xs"
            onChange={(e) => {
              setQuery(e.target.value);
              if (!e.target.value) term.clearFind();
            }}
            onKeyDown={(e) => {
              if (e.key !== "Enter" || !query) return;
              e.preventDefault();
              term.find(query, e.shiftKey);
            }}
          />
        )}
      </div>
      {!text && <p className="text-xs text-muted-foreground">No output yet.</p>}
      {failed ? (
        <pre className="min-h-0 flex-1 overflow-auto rounded-md bg-surface px-3 py-2 font-mono text-[11px] leading-relaxed">
          {text.replace(SGR, "")}
        </pre>
      ) : (
        <div
          ref={host}
          data-testid="background-output"
          className="min-h-0 flex-1 overflow-hidden rounded-md bg-surface py-2 pl-3"
        />
      )}
    </div>
  );
}

const RETURNED_MS = 1600;

function BackgroundList({
  calls,
  sessionId,
  returned,
  onOpen,
}: {
  calls: readonly BackgroundCall[];
  sessionId: string | null;
  returned: string | null;
  onOpen: (key: string) => void;
}) {
  const content = useRef<HTMLDivElement>(null);
  const listed = calls.length > 0;
  /* the list's place outlives it, as the log's does: opening a call, another
     tab, the dock closed or another session */
  useLayoutEffect(() => {
    const viewport = content.current?.closest<HTMLElement>(
      "[data-slot=scroll-area-viewport]",
    );
    if (!viewport || !listed) return;
    viewport.scrollTop = backgroundPlace(sessionId).listTop;
    /* a log opened from the transcript may be of a row out of view */
    content.current
      ?.querySelector("[data-returned]")
      ?.scrollIntoView?.({ block: "nearest" });
    const keep = () =>
      rememberBackgroundPlace(sessionId, { listTop: viewport.scrollTop });
    viewport.addEventListener("scroll", keep, { passive: true });
    return () => viewport.removeEventListener("scroll", keep);
  }, [sessionId, listed]);
  if (!listed)
    return (
      <PanelEmpty icon={Terminal} title="Nothing running">
        Commands and jobs the agent leaves running show here.
      </PanelEmpty>
    );
  return (
    <ScrollArea className="h-full">
      <div ref={content} className="px-3 py-2">
        <BackgroundGroup
          calls={calls}
          sessionId={sessionId}
          returned={returned}
          onOpen={onOpen}
        />
      </div>
    </ScrollArea>
  );
}

export function BackgroundSurface({ sessionId }: SurfaceContext) {
  const calls = useBackgroundCalls(sessionId);
  /* whose output is open; the dock can stay up while another session opens */
  const [held, setHeld] = useState(() => ({
    session: sessionId,
    log: backgroundPlace(sessionId).log,
  }));
  if (held.session !== sessionId)
    setHeld({ session: sessionId, log: backgroundPlace(sessionId).log });
  const log = calls.find((call) => call.key === held.log) ?? null;
  const show = (key: string | null) => {
    rememberBackgroundPlace(sessionId, { log: key, above: 0 });
    setHeld({ session: sessionId, log: key });
  };
  useAskedLog(
    sessionId,
    useCallback(
      (key: string) => {
        rememberBackgroundPlace(sessionId, { log: key, above: 0 });
        setHeld({ session: sessionId, log: key });
      },
      [sessionId],
    ),
  );
  const [returned, setReturned] = useState<string | null>(null);
  useEffect(() => {
    if (!returned) return;
    const done = setTimeout(() => setReturned(null), RETURNED_MS);
    return () => clearTimeout(done);
  }, [returned]);
  if (log)
    return (
      <BackgroundLog
        call={log}
        sessionId={sessionId}
        onBack={() => {
          setReturned(log.key);
          show(null);
        }}
      />
    );
  return (
    <BackgroundList
      calls={calls}
      sessionId={sessionId}
      returned={returned}
      onOpen={show}
    />
  );
}
