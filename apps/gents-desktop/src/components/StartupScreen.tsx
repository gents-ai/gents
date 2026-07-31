import { useEffect, useState } from "react";

import type { DesktopStartupPhase } from "../hooks/useDesktopShell";
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
  onRetry: () => Promise<void>;
  phase: Exclude<DesktopStartupPhase, "ready">;
};

type StepState = "active" | "complete" | "pending" | "error";

export function StartupScreen({ error, onRetry, phase }: StartupScreenProps) {
  const [asideIndex, setAsideIndex] = useState(0);
  const [syncing, setSyncing] = useState(false);
  const failed = phase === "configuration-error" || phase === "client-error";

  useEffect(() => {
    const interval = window.setInterval(() => {
      setAsideIndex((current) => (current + 1) % STARTUP_ASIDES.length);
    }, 2200);
    return () => window.clearInterval(interval);
  }, []);

  useEffect(() => {
    setSyncing(false);
    if (phase !== "starting-client") return;
    const timeout = window.setTimeout(() => setSyncing(true), 900);
    return () => window.clearTimeout(timeout);
  }, [phase]);

  let connectionState: StepState = "pending";
  let clientState: StepState = "pending";
  let syncState: StepState = "pending";

  if (phase === "loading-configuration") {
    connectionState = "active";
  } else if (phase === "starting-client") {
    connectionState = "complete";
    clientState = syncing ? "complete" : "active";
    syncState = syncing ? "active" : "pending";
  } else if (phase === "configuration-error") {
    connectionState = "error";
  } else {
    connectionState = "complete";
    clientState = "error";
  }

  const activeLabel =
    phase === "loading-configuration"
      ? "Reading saved connections"
      : phase === "starting-client" && syncing
        ? "Synchronizing agent state"
        : phase === "starting-client"
          ? "Starting the secure client"
          : "Startup paused";

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
          <h2 id="startup-title">
            {failed ? "Startup paused" : "Bringing Gents online"}
          </h2>
          <p aria-live="polite" className="startup-current-status">
            {activeLabel}
            {!failed ? <span aria-hidden="true" className="startup-ellipsis" /> : null}
          </p>
          {!failed ? (
            <p aria-hidden="true" className="startup-aside" key={asideIndex}>
              {STARTUP_ASIDES[asideIndex]}
            </p>
          ) : null}
        </div>

        <ol aria-label="Startup progress" className="startup-steps">
          <StartupStep label="Read saved connections" state={connectionState} />
          <StartupStep label="Start secure client" state={clientState} />
          <StartupStep label="Synchronize agent state" state={syncState} />
        </ol>

        {failed ? (
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

function StartupStep({ label, state }: { label: string; state: StepState }) {
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
