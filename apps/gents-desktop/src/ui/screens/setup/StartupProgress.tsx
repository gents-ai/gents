import {
  Fragment,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { CircleAlert, CircleCheck } from "lucide-react";

import { Button } from "@gents/ui/components/button";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { Mark } from "@/app/Mark";
import type { LoadingStepState, StartupStep } from "../../../lib/loadingStatus";
import {
  describeManagedServerWait,
  type ManagedServerWait,
} from "../../../lib/managedServerStartup";

export const SETUP_COMPLETE_DWELL_MS = 2_000;
export const STEP_DWELL_MS = 800;

export type StartupProgressStep = StartupStep & { detail?: string | null };

export type StartupSummary = {
  title: string;
  facts: {
    label: string;
    value: string;
    full?: string;
  }[];
};

const ASIDES = [
  "Catalyzing dilithium converters.",
  "Configuring the human-computer interface.",
  "Waking up the nodes.",
  "Immanentizing the eschaton.",
  "Teaching the gossip network some manners.",
  "Aligning the durable timelines.",
];

function useAside() {
  const [index, setIndex] = useState(0);
  useEffect(() => {
    const interval = window.setInterval(
      () => setIndex((current) => (current + 1) % ASIDES.length),
      2200,
    );
    return () => window.clearInterval(interval);
  }, []);
  return { index, text: ASIDES[index] };
}

function usePaced(target: string, immediate: boolean): string {
  const [shown, setShown] = useState(target);
  const since = useRef(Date.now());
  useEffect(() => {
    if (target === shown) return;
    const wait = immediate
      ? 0
      : Math.max(0, since.current + STEP_DWELL_MS - Date.now());
    const timer = window.setTimeout(() => {
      since.current = Date.now();
      setShown(target);
    }, wait);
    return () => window.clearTimeout(timer);
  }, [target, shown, immediate]);
  return immediate ? target : shown;
}

const HANDOVER_MS = 300;
function useLeaving(shown: string): string | null {
  const [leaving, setLeaving] = useState<string | null>(null);
  const previous = useRef(shown);
  useEffect(() => {
    if (previous.current === shown) return;
    setLeaving(previous.current);
    previous.current = shown;
    const timer = window.setTimeout(() => setLeaving(null), HANDOVER_MS);
    return () => window.clearTimeout(timer);
  }, [shown]);
  return leaving === shown ? null : leaving;
}

function useShownHeight(shownKey: string) {
  const card = useRef<HTMLDivElement | null>(null);
  const [height, setHeight] = useState<number | null>(null);
  useLayoutEffect(() => {
    const el = card.current;
    if (!el) return;
    setHeight(el.offsetHeight);
    const observer = new ResizeObserver(() => setHeight(el.offsetHeight));
    observer.observe(el);
    return () => observer.disconnect();
  }, [shownKey]);
  return { height, card };
}

const DONE = "done";

function targetOf(steps: StartupProgressStep[], failed: boolean, done: boolean) {
  if (done) return DONE;
  const at =
    steps.find((s) => s.state === (failed ? "error" : "active")) ??
    steps.find((s) => s.state !== "complete") ??
    steps[steps.length - 1];
  return at?.key ?? DONE;
}

const stepIcon = (state: LoadingStepState) =>
  state === "complete" ? (
    <CircleCheck className="size-4 shrink-0 text-muted-foreground" />
  ) : state === "active" ? (
    <Spinner className="shrink-0 text-foreground" />
  ) : state === "error" ? (
    <CircleAlert className="size-4 shrink-0 text-destructive" />
  ) : (
    <span className="mx-[5px] size-1.5 shrink-0 rounded-full bg-border" />
  );

function Card({
  leaving = false,
  icon,
  cardRef,
  children,
}: {
  leaving?: boolean;
  icon: ReactNode;
  cardRef?: React.Ref<HTMLDivElement>;
  children: ReactNode;
}) {
  return (
    <div
      ref={cardRef}
      aria-hidden={leaving || undefined}
      inert={leaving}
      data-testid={leaving ? undefined : "startup-step"}
      className={cn(
        "absolute inset-x-0 top-0 flex items-start gap-3 rounded-2xl border border-border/60 bg-raised px-4 py-3 text-sm duration-300 ease-out motion-reduce:animate-none",
        leaving
          ? "pointer-events-none animate-out fill-mode-forwards fade-out-0 slide-out-to-top-1.5"
          : "animate-in fill-mode-both fade-in-0 slide-in-from-bottom-1.5",
      )}
    >
      <span className="grid min-w-0 flex-1 gap-0.5">{children}</span>
      <span className="flex h-5 items-center">{icon}</span>
    </div>
  );
}

export function StartupProgress({
  title,
  doneTitle = title,
  titleId,
  steps,
  failedLabel,
  done = false,
  wait = null,
  onOpenLoginItems,
  waitTestId,
  loginItemsTestId,
  waitActions,
  error = null,
  diagnosticsHint = null,
  actions,
  footer,
  onFinished,
  summary,
}: {
  title: string;
  doneTitle?: string;
  titleId?: string;
  steps: StartupProgressStep[];
  failedLabel?: string;
  done?: boolean;
  wait?: ManagedServerWait | null;
  onOpenLoginItems?: () => Promise<void>;
  waitTestId: string;
  loginItemsTestId: string;
  waitActions?: ReactNode;
  error?: string | null;
  diagnosticsHint?: string | null;
  actions?: ReactNode;
  footer?: ReactNode;
  onFinished?: () => void;
  summary?: StartupSummary;
}) {
  const failed = steps.some((s) => s.state === "error");
  const shownKey = usePaced(targetOf(steps, failed, done), failed);
  const shownAt = steps.findIndex((s) => s.key === shownKey);
  const waiting = useManagedServerWaitDescription(wait);
  const aside = useAside();
  const finished = shownKey === DONE;
  const working = !failed && !finished;
  const { height, card } = useShownHeight(shownKey);
  const leavingKey = useLeaving(shownKey);
  const told = useRef(onFinished);
  told.current = onFinished;
  useEffect(() => {
    if (finished) told.current?.();
  }, [finished]);
  const details = steps.flatMap((s) => (s.detail ? [s.detail] : []));
  return (
    <div className="grid gap-6">
      <div className="grid gap-6">
        <Mark className="h-5 w-7 text-ink" />
        <h1 id={titleId} className="font-heading text-xl font-medium text-heading">
          {finished ? doneTitle : title}
        </h1>
      </div>
      <div className="grid gap-3">
        <div
          aria-live="polite"
          className="relative transition-[height] duration-300 ease-out motion-reduce:transition-none"
          style={height === null ? undefined : { height }}
        >
          {[leavingKey, shownKey].map((key) => {
            if (key === null) return null;
            const leaving = key !== shownKey;
            if (key === DONE) {
              return (
                <Card
                  key={key}
                  leaving={leaving}
                  cardRef={leaving ? undefined : card}
                  icon={stepIcon("complete")}
                >
                  {summary ? (
                    <>
                      <span>{summary.title}</span>
                      {summary.facts.length > 0 ? (
                        <dl className="mt-1 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-0.5 text-xs">
                          {summary.facts.map((fact) => (
                            <Fragment key={fact.label}>
                              <dt className="text-muted-foreground">{fact.label}</dt>
                              <dd
                                className="truncate font-mono"
                                title={fact.full ?? fact.value}
                              >
                                {fact.value}
                              </dd>
                            </Fragment>
                          ))}
                        </dl>
                      ) : null}
                    </>
                  ) : (
                    <>
                      <span>Everything started.</span>
                      {details.map((detail) => (
                        <span
                          key={detail}
                          className="text-xs break-words text-muted-foreground"
                        >
                          {detail}
                        </span>
                      ))}
                    </>
                  )}
                </Card>
              );
            }
            const step = steps.find((s) => s.key === key);
            if (!step) return null;
            const state = leaving && step.state === "active" ? "complete" : step.state;
            const text =
              state === "error"
                ? (failedLabel ?? step.doing)
                : state === "complete"
                  ? step.label
                  : step.key === "node" && waiting && state === "active"
                    ? waiting.label
                    : step.doing;
            return (
              <Card
                key={key}
                leaving={leaving}
                cardRef={leaving ? undefined : card}
                icon={stepIcon(state)}
              >
                <span data-state={state}>{text}</span>
                {step.detail && state !== "pending" ? (
                  <span className="text-xs break-words text-muted-foreground">
                    {step.detail}
                  </span>
                ) : null}
              </Card>
            );
          })}
        </div>
        <div aria-hidden="true" className="flex gap-1.5">
          {steps.map((step, i) => {
            const state =
              finished || i < shownAt
                ? "complete"
                : i === shownAt
                  ? step.state
                  : "pending";
            return (
              <span
                key={step.key}
                data-state={state}
                className={cn(
                  "h-1 w-6 rounded-full transition-colors duration-300",
                  state === "complete"
                    ? "bg-brand"
                    : state === "active"
                      ? "bg-foreground/40"
                      : state === "error"
                        ? "bg-destructive"
                        : "bg-border",
                )}
              />
            );
          })}
        </div>
        {working && !wait ? (
          <p
            aria-hidden="true"
            className="min-h-[1.5em] font-mono text-sm text-brand"
            key={aside.index}
          >
            {aside.text}
          </p>
        ) : null}
      </div>
      {wait ? (
        <ManagedServerWaitNotice
          wait={wait}
          testId={waitTestId}
          loginItemsTestId={loginItemsTestId}
          onOpenLoginItems={onOpenLoginItems}
        >
          {waitActions}
        </ManagedServerWaitNotice>
      ) : null}
      {error ? (
        <div className="grid gap-3">
          <p className="text-sm text-destructive">{error}</p>
          <DiagnosticsHint hint={diagnosticsHint} />
          {actions ? <div className="flex flex-wrap gap-2">{actions}</div> : null}
        </div>
      ) : null}
      {finished && !error && footer ? <div>{footer}</div> : null}
    </div>
  );
}

export function DiagnosticsHint({ hint }: { hint: string | null | undefined }) {
  if (!hint) return null;
  return (
    <p
      className="text-xs break-words text-muted-foreground"
      data-testid="diagnostics-hint"
    >
      Logs: <span className="font-mono">{hint}</span>
    </p>
  );
}

function useManagedServerWaitDescription(wait: ManagedServerWait | null) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!wait) return;
    setNow(Date.now());
    const interval = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(interval);
  }, [wait]);
  return wait ? describeManagedServerWait(wait, Math.max(now, wait.since)) : null;
}

export function ManagedServerWaitNotice({
  wait,
  onOpenLoginItems,
  testId,
  loginItemsTestId,
  className,
  children,
}: {
  wait: ManagedServerWait;
  onOpenLoginItems?: () => Promise<void>;
  testId: string;
  loginItemsTestId: string;
  className?: string;
  children?: ReactNode;
}) {
  const waiting = useManagedServerWaitDescription(wait)!;
  return (
    <div className={cn("grid gap-3", className)} data-testid={testId}>
      <p className="text-sm text-muted-foreground">{waiting.detail}</p>
      <div className="flex flex-wrap gap-2 empty:hidden">
        {wait.kind === "approval" && onOpenLoginItems ? (
          <Button
            data-testid={loginItemsTestId}
            variant="brand"
            onClick={() => void onOpenLoginItems()}
          >
            Open Login Items settings
          </Button>
        ) : null}
        {children}
      </div>
    </div>
  );
}
