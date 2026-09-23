import { useEffect, useState } from "react";
import { ArrowRight, CircleAlert, CircleCheck } from "lucide-react";

import { Button } from "@gents/ui/components/button";
import { Spinner } from "@gents/ui/components/spinner";
import type { LoadingStepState } from "../../../lib/loadingStatus";
import {
  describeManagedServerWait,
  type ManagedServerWait,
} from "../../../lib/managedServerStartup";

export const SETUP_COMPLETE_DWELL_MS = 2_000;

export type SetupProgressStep = {
  label: string;
  state: LoadingStepState | null;
  detail: string | null;
};

const stepIcon = (state: LoadingStepState | null) =>
  state === "complete" ? (
    <CircleCheck className="size-4 shrink-0 text-muted-foreground" />
  ) : state === "active" ? (
    <Spinner className="shrink-0 text-foreground" />
  ) : state === "error" ? (
    <CircleAlert className="size-4 shrink-0 text-destructive" />
  ) : (
    <span className="size-1.5 shrink-0 rounded-full bg-border" />
  );

export function SetupProgress({
  title,
  label,
  failed,
  done,
  steps,
  wait,
  error,
  onRetry,
  onContinue,
  onOpenLoginItems,
}: {
  title: string;
  label: string;
  failed: boolean;
  done: boolean;
  steps: SetupProgressStep[];
  wait: ManagedServerWait | null;
  error: string | null;
  onRetry: () => void;
  onContinue: () => void;
  onOpenLoginItems?: () => Promise<void>;
}) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!wait) return;
    setNow(Date.now());
    const interval = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(interval);
  }, [wait]);
  const waiting = wait
    ? describeManagedServerWait(wait, Math.max(now, wait.since))
    : null;

  return (
    <>
      <h1 className="font-heading text-2xl font-medium text-heading">{title}</h1>
      <p
        aria-live="polite"
        className="mt-3 flex items-center gap-2 text-sm text-muted-foreground"
      >
        {failed || done ? null : <Spinner className="text-foreground" />}
        {waiting?.label ?? label}
      </p>
      <ol aria-label="Setup progress" className="mt-6 grid gap-2">
        {steps.map(({ label: stepLabel, state, detail }, i) =>
          state === "pending" ? null : (
            <li
              key={stepLabel}
              data-state={state ?? "pending"}
              className="flex items-start gap-3 rounded-2xl border border-border/60 bg-raised px-4 py-3 text-sm animate-in fade-in-0 slide-in-from-bottom-1 duration-300 fill-mode-both"
            >
              <span className="pt-0.5 font-mono text-[11px] text-muted-foreground">
                0{i + 1}.
              </span>
              <span className="grid min-w-0 flex-1 gap-0.5">
                <span>{stepLabel}</span>
                {detail ? (
                  <span className="text-xs break-words text-muted-foreground">
                    {detail}
                  </span>
                ) : null}
              </span>
              {stepIcon(state)}
            </li>
          ),
        )}
      </ol>
      {waiting && wait ? (
        <div className="mt-4 grid gap-3" data-testid="setup-managed-server-wait">
          <p className="text-sm text-muted-foreground">{waiting.detail}</p>
          {wait.kind === "approval" && onOpenLoginItems ? (
            <Button
              className="w-fit"
              data-testid="setup-open-login-items"
              variant="brand"
              onClick={() => void onOpenLoginItems()}
            >
              Open Login Items settings
            </Button>
          ) : null}
        </div>
      ) : null}
      {error ? (
        <div className="mt-4 grid gap-3">
          <p className="text-sm text-destructive">{error}</p>
          <Button variant="brand" onClick={onRetry}>
            Try again
          </Button>
        </div>
      ) : null}
      {done && !error ? (
        <div className="mt-6">
          <Button data-testid="setup-continue" variant="brand" onClick={onContinue}>
            Continue <ArrowRight />
          </Button>
        </div>
      ) : null}
    </>
  );
}
