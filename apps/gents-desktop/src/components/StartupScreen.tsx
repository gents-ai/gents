import { useEffect, useState } from "react";

import {
  projectStartupLoadingStatus,
  type DesktopStartupPhase,
  type LoadingStepState,
} from "../lib/loadingStatus";
import { Mark } from "../ui/app/Mark";

const STARTUP_ASIDES = [
  "Catalyzing dilithium converters.",
  "Configuring the human-computer interface.",
  "Waking up the agents.",
  "Immanentizing the eschaton.",
  "Teaching the gossip network some manners.",
  "Aligning the durable timelines.",
];

type StartupScreenProps = {
  error: string | null;
  managedServerSupported?: boolean;
  onRetry: () => Promise<void>;
  phase: Exclude<DesktopStartupPhase, "ready">;
};

export function StartupScreen({
  error,
  managedServerSupported = false,
  onRetry,
  phase,
}: StartupScreenProps) {
  const [asideIndex, setAsideIndex] = useState(0);
  const status = projectStartupLoadingStatus(phase, managedServerSupported);

  useEffect(() => {
    const interval = window.setInterval(() => {
      setAsideIndex((current) => (current + 1) % STARTUP_ASIDES.length);
    }, 2200);
    return () => window.clearInterval(interval);
  }, []);

  return (
    <section
      aria-labelledby="startup-title"
      className="viewport-frame grid place-items-center bg-background px-8 text-foreground"
      data-testid="startup-screen"
    >
      <div className="grid w-full max-w-md gap-8">
        <Mark className="h-4 text-ink" />

        <div className="grid gap-2">
          <p className="font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
            System startup
          </p>
          <h2 id="startup-title" className="font-heading text-2xl font-medium text-heading">
            {status.title}
          </h2>
          <p aria-live="polite" className="text-sm font-medium">
            {status.currentLabel}
            {!status.failed ? (
              <span aria-hidden="true" className="startup-ellipsis" />
            ) : null}
          </p>
          {!status.failed ? (
            <p
              aria-hidden="true"
              className="min-h-[1.5em] font-mono text-sm text-brand"
              key={asideIndex}
            >
              {STARTUP_ASIDES[asideIndex]}
            </p>
          ) : null}
        </div>

        <ol aria-label="Startup progress" className="grid gap-2">
          {status.managedServerState ? (
            <StartupStep
              label="Restore hosted agent"
              state={status.managedServerState}
            />
          ) : null}
          <StartupStep label="Read saved connections" state={status.connectionState} />
          <StartupStep label="Start secure client" state={status.clientState} />
        </ol>

        {status.failed ? (
          <div className="grid gap-3">
            <p className="text-sm text-destructive">
              {error ?? "Gents could not finish starting."}
            </p>
            <button
              className="inline-flex h-8 w-fit items-center rounded-lg bg-brand px-3 text-sm font-medium text-brand-foreground"
              data-testid="startup-retry"
              onClick={() => void onRetry()}
              type="button"
            >
              Try again
            </button>
          </div>
        ) : null}
      </div>
    </section>
  );
}

function StartupStep({ label, state }: { label: string; state: LoadingStepState }) {
  return (
    <li
      className="grid min-h-[42px] grid-cols-[12px_minmax(0,1fr)_auto] items-center gap-4 rounded-lg border border-border/60 px-4 py-3 text-sm text-muted-foreground"
      data-state={state}
    >
      <span
        aria-hidden="true"
        className={
          state === "complete"
            ? "size-2 rounded-full bg-brand"
            : state === "active"
              ? "size-2 rounded-full bg-foreground"
              : state === "error"
                ? "size-2 rounded-full bg-destructive"
                : "size-2 rounded-full bg-border"
        }
      />
      <span>{label}</span>
      <span>
        {state === "complete"
          ? "Ready"
          : state === "active"
            ? "Working"
            : state === "error"
              ? "Needs attention"
              : "Queued"}
      </span>
    </li>
  );
}
