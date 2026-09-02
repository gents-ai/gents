import { useEffect, useState } from "react";

import {
  projectStartupLoadingStatus,
  type DesktopStartupPhase,
  type LoadingStepState,
} from "../lib/loadingStatus";
import { BrandLockup } from "./fleet/BrandLockup";

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
      className="startup-screen"
      data-testid="startup-screen"
    >
      <div className="startup-card panel">
        <BrandLockup />

        <div className="startup-heading">
          <p className="eyebrow">System startup</p>
          <h2 id="startup-title">{status.title}</h2>
          <p aria-live="polite" className="startup-current-status">
            {status.currentLabel}
            {!status.failed ? (
              <span aria-hidden="true" className="startup-ellipsis" />
            ) : null}
          </p>
          {!status.failed ? (
            <p aria-hidden="true" className="startup-aside" key={asideIndex}>
              {STARTUP_ASIDES[asideIndex]}
            </p>
          ) : null}
        </div>

        <ol aria-label="Startup progress" className="startup-steps">
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
          <div className="startup-recovery">
            <p className="startup-error">
              {error ?? "Gents could not finish starting."}
            </p>
            <button
              className="primary-button"
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
    <li className="startup-step" data-state={state}>
      <span aria-hidden="true" className="startup-step-marker" />
      <span>{label}</span>
      <span className="startup-step-state">
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
