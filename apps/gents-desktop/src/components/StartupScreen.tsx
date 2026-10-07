import { useEffect, useState } from "react";
import { Button } from "@gents/ui/components/button";

import {
  projectStartupLoadingStatus,
  type LoadingStepState,
} from "../lib/loadingStatus";
import { describeManagedServerWait } from "../lib/managedServerStartup";
import { useApp } from "../ui/app/AppContext";
import { Mark } from "../ui/app/Mark";
import { useStartup } from "../ui/hooks/useClient";
import { useNow } from "../ui/lib/clock";
import { DiagnosticsHint } from "../ui/screens/setup/SetupProgress";

const STARTUP_ASIDES = [
  "Catalyzing dilithium converters.",
  "Configuring the human-computer interface.",
  "Waking up the agents.",
  "Immanentizing the eschaton.",
  "Teaching the gossip network some manners.",
  "Aligning the durable timelines.",
];

/* one aside after another while startup is under way */
function useAside() {
  const [index, setIndex] = useState(0);
  useEffect(() => {
    const interval = window.setInterval(
      () => setIndex((current) => (current + 1) % STARTUP_ASIDES.length),
      2200,
    );
    return () => window.clearInterval(interval);
  }, []);
  return { index, text: STARTUP_ASIDES[index] };
}

/** Where startup has got to, until the client is ready: each step, the
    managed server's wait, and on failure the ways on. */
export function StartupScreen() {
  const { api, lifecycle } = useApp();
  const startup = useStartup();
  const aside = useAside();
  const waiting = startup.managedServerWait !== null;
  const now = useNow(waiting);
  if (startup.phase === "ready") return null;
  const { phase, managedServerWait } = startup;
  const status = projectStartupLoadingStatus(phase, lifecycle.supportsManagedServer);
  const wait =
    managedServerWait && phase === "checking-managed-server"
      ? describeManagedServerWait(
          managedServerWait,
          Math.max(now, managedServerWait.since),
        )
      : null;
  const openLoginItems = api.openManagedServerLoginItems;
  const skip = (testId: string) => (
    <Button
      variant="outline"
      data-testid={testId}
      onClick={lifecycle.skipManagedServerWait}
    >
      Continue without the local agent
    </Button>
  );

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
            {!status.failed && <span aria-hidden="true" className="startup-ellipsis" />}
          </p>
          {wait && (
            <div className="grid gap-3" data-testid="startup-managed-server-wait">
              <p className="text-sm text-muted-foreground">{wait.detail}</p>
              <div className="flex flex-wrap gap-2">
                {managedServerWait?.kind === "approval" && openLoginItems && (
                  <Button
                    variant="brand"
                    data-testid="startup-open-login-items"
                    onClick={() => void openLoginItems()}
                  >
                    Open Login Items settings
                  </Button>
                )}
                {skip("startup-skip-managed-server-wait")}
              </div>
            </div>
          )}
          {!wait && !status.failed && (
            <p
              aria-hidden="true"
              className="min-h-[1.5em] font-mono text-sm text-brand"
              key={aside.index}
            >
              {aside.text}
            </p>
          )}
        </div>

        <ol aria-label="Startup progress" className="grid gap-2">
          {status.managedServerState && (
            <StartupStep label="Check local agent" state={status.managedServerState} />
          )}
          <StartupStep label="Read saved connections" state={status.connectionState} />
          <StartupStep label="Start secure client" state={status.clientState} />
        </ol>

        {status.failed && (
          <div className="grid gap-3">
            <p className="text-sm text-destructive">
              {startup.error ?? "Gents could not finish starting."}
            </p>
            <DiagnosticsHint hint={startup.diagnosticsHint} />
            <div className="flex flex-wrap gap-2">
              <Button
                variant="brand"
                data-testid="startup-retry"
                onClick={() => void lifecycle.retryStartup()}
              >
                Try again
              </Button>
              {phase === "managed-server-error" && (
                <>
                  {startup.canRestartManagedServer && (
                    <Button
                      variant="outline"
                      data-testid="startup-restart-managed-server"
                      onClick={() => void lifecycle.restartManagedServer()}
                    >
                      Restart agent
                    </Button>
                  )}
                  {skip("startup-continue-without-managed-server")}
                </>
              )}
            </div>
          </div>
        )}
      </div>
    </section>
  );
}

const STEP: Record<LoadingStepState, { dot: string; label: string }> = {
  complete: { dot: "bg-brand", label: "Ready" },
  active: { dot: "bg-foreground", label: "Working" },
  error: { dot: "bg-destructive", label: "Needs attention" },
  pending: { dot: "bg-border", label: "Queued" },
};

function StartupStep({ label, state }: { label: string; state: LoadingStepState }) {
  const step = STEP[state];
  return (
    <li
      className="grid min-h-[42px] grid-cols-[12px_minmax(0,1fr)_auto] items-center gap-4 rounded-lg border border-border/60 px-4 py-3 text-sm text-muted-foreground"
      data-state={state}
    >
      <span aria-hidden="true" className={`size-2 rounded-full ${step.dot}`} />
      <span>{label}</span>
      <span>{step.label}</span>
    </li>
  );
}
