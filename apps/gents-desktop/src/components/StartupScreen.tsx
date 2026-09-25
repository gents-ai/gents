import { useEffect, useState } from "react";

import {
  projectStartupLoadingStatus,
  type DesktopStartupPhase,
  type LoadingStepState,
} from "../lib/loadingStatus";
import {
  describeManagedServerWait,
  type ManagedServerWait,
} from "../lib/managedServerStartup";
import { Mark } from "../ui/app/Mark";
import { DiagnosticsHint } from "../ui/screens/setup/SetupProgress";
import type { ManagedServerResetResult } from "@source-inc/gents-desktop-client";

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
  managedServerReset?: ManagedServerResetResult | null;
  onResetManagedServer?: () => Promise<void>;
  managedServerWait?: ManagedServerWait | null;
  onSkipManagedServerWait?: () => void;
  onOpenLoginItems?: () => Promise<void>;
  onRestartManagedServer?: () => Promise<void>;
  diagnosticsHint?: string | null;
};

export function StartupScreen({
  error,
  managedServerSupported = false,
  onRetry,
  phase,
  managedServerReset = null,
  onResetManagedServer,
  managedServerWait = null,
  onSkipManagedServerWait,
  onOpenLoginItems,
  onRestartManagedServer,
  diagnosticsHint = null,
}: StartupScreenProps) {
  const [asideIndex, setAsideIndex] = useState(0);
  const [resetConfirmed, setResetConfirmed] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const status = projectStartupLoadingStatus(phase, managedServerSupported);
  const wait =
    managedServerWait && phase === "checking-managed-server"
      ? describeManagedServerWait(
          managedServerWait,
          Math.max(now, managedServerWait.since),
        )
      : null;

  useEffect(() => {
    if (!managedServerWait) return;
    setNow(Date.now());
    const interval = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(interval);
  }, [managedServerWait]);

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
          <h2
            id="startup-title"
            className="font-heading text-2xl font-medium text-heading"
          >
            {status.title}
          </h2>
          <p aria-live="polite" className="text-sm font-medium">
            {wait?.label ?? status.currentLabel}
            {!status.failed ? (
              <span aria-hidden="true" className="startup-ellipsis" />
            ) : null}
          </p>
          {wait ? (
            <div className="grid gap-3" data-testid="startup-managed-server-wait">
              <p className="text-sm text-muted-foreground">{wait.detail}</p>
              <div className="flex flex-wrap gap-2">
                {managedServerWait?.kind === "approval" && onOpenLoginItems ? (
                  <button
                    className="inline-flex h-8 w-fit items-center rounded-lg bg-brand px-3 text-sm font-medium text-brand-foreground"
                    data-testid="startup-open-login-items"
                    onClick={() => void onOpenLoginItems()}
                    type="button"
                  >
                    Open Login Items settings
                  </button>
                ) : null}
                {onSkipManagedServerWait ? (
                  <button
                    className="inline-flex h-8 w-fit items-center rounded-lg border border-border px-3 text-sm font-medium"
                    data-testid="startup-skip-managed-server-wait"
                    onClick={onSkipManagedServerWait}
                    type="button"
                  >
                    Continue without the local agent
                  </button>
                ) : null}
              </div>
            </div>
          ) : !status.failed ? (
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
            <StartupStep label="Check local agent" state={status.managedServerState} />
          ) : null}
          <StartupStep label="Read saved connections" state={status.connectionState} />
          <StartupStep label="Start secure client" state={status.clientState} />
        </ol>

        {status.failed ? (
          <div className="grid gap-3">
            <p className="text-sm text-destructive">
              {error ?? "Gents could not finish starting."}
            </p>
            <DiagnosticsHint hint={diagnosticsHint} />
            {phase === "managed-server-error" && managedServerReset ? (
              <div className="grid gap-3 rounded-lg border border-destructive/40 p-3 text-sm">
                <p>
                  This installation contains an incompatible local database at{" "}
                  <code className="break-all">{managedServerReset.dataPath}</code>.
                </p>
                <p>{managedServerReset.consequence}</p>
                <label className="flex items-start gap-2">
                  <input
                    checked={resetConfirmed}
                    data-testid="managed-server-reset-confirmation"
                    onChange={(event) => setResetConfirmed(event.currentTarget.checked)}
                    type="checkbox"
                  />
                  <span>
                    Archive this exact managed home and initialize a new local database:
                    <code className="block break-all">
                      {managedServerReset.managedHome}
                    </code>
                  </span>
                </label>
                <button
                  className="inline-flex h-8 w-fit items-center rounded-lg bg-destructive px-3 text-sm font-medium text-destructive-foreground disabled:opacity-50"
                  data-testid="managed-server-reset"
                  disabled={!resetConfirmed || !onResetManagedServer}
                  onClick={() => void onResetManagedServer?.()}
                  type="button"
                >
                  Archive old data and start fresh
                </button>
              </div>
            ) : null}
            <div className="flex flex-wrap gap-2">
              <button
                className="inline-flex h-8 w-fit items-center rounded-lg bg-brand px-3 text-sm font-medium text-brand-foreground"
                data-testid="startup-retry"
                onClick={() => void onRetry()}
                type="button"
              >
                Try again
              </button>
              {phase === "managed-server-error" && !managedServerReset ? (
                <>
                  {onRestartManagedServer ? (
                    <button
                      className="inline-flex h-8 w-fit items-center rounded-lg border border-border px-3 text-sm font-medium"
                      data-testid="startup-restart-managed-server"
                      onClick={() => void onRestartManagedServer()}
                      type="button"
                    >
                      Restart agent
                    </button>
                  ) : null}
                  {onSkipManagedServerWait ? (
                    <button
                      className="inline-flex h-8 w-fit items-center rounded-lg border border-border px-3 text-sm font-medium"
                      data-testid="startup-continue-without-managed-server"
                      onClick={onSkipManagedServerWait}
                      type="button"
                    >
                      Continue without the local agent
                    </button>
                  ) : null}
                </>
              ) : null}
            </div>
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
