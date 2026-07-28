import { BackendHealthRow } from "./BackendHealthRow.js";
import type {
  BackendDisplayState,
  BackendHealth,
} from "@source-inc/gents-desktop-client";
import { useBackendHealth } from "./useBackendHealth.js";

type ChipTone = "success" | "warning" | "error" | "neutral";

function FleetSummary({ backends }: { backends: BackendHealth[] }) {
  const counts: Record<BackendDisplayState, number> = {
    available: 0,
    unhealthy: 0,
    stale: 0,
    "rate-limited": 0,
    "circuit-open": 0,
    unknown: 0,
    disabled: 0,
  };
  for (const b of backends) counts[b.displayState]++;
  const total = backends.length;

  const chips: { tone: ChipTone; count: number; label: string }[] = [];
  if (total === 0) {
    chips.push({ tone: "neutral", count: 0, label: "backends" });
  } else {
    chips.push({ tone: "neutral", count: total, label: "registered" });
    if (counts.available)
      chips.push({
        tone: "success",
        count: counts.available,
        label: "available",
      });
    if (counts.unhealthy)
      chips.push({
        tone: "error",
        count: counts.unhealthy,
        label: "unhealthy",
      });
    if (counts.stale)
      chips.push({ tone: "warning", count: counts.stale, label: "stale" });
    if (counts["rate-limited"])
      chips.push({
        tone: "warning",
        count: counts["rate-limited"],
        label: "rate-limited",
      });
    if (counts["circuit-open"])
      chips.push({
        tone: "warning",
        count: counts["circuit-open"],
        label: "circuit-open",
      });
    if (counts.unknown)
      chips.push({ tone: "neutral", count: counts.unknown, label: "unknown" });
    if (counts.disabled)
      chips.push({
        tone: "neutral",
        count: counts.disabled,
        label: "disabled",
      });
  }

  return (
    <div className="backend-health__fleet-summary" aria-live="polite">
      {chips.map((c) => (
        <span
          key={`${c.label}-${c.count}`}
          className="backend-health__summary-chip"
          data-tone={c.tone}
        >
          <span className="backend-health__summary-count">{c.count}</span>{" "}
          {c.label}
        </span>
      ))}
    </div>
  );
}

function EmptyFleet() {
  return (
    <div className="backend-health__empty-fleet">
      <div className="backend-health__empty-icon" aria-hidden="true">
        ∅
      </div>
      <h2 className="backend-health__empty-heading">No backends registered</h2>
      <p className="backend-health__empty-body">
        Add an <code>InferenceBackend</code> document to the control plane to
        register a provider. The runtime reads from the
        <code> InferenceBackend</code> collection at start-up and on every
        control-doc update.
      </p>
    </div>
  );
}

export function BackendHealthPanel({
  initialBackends,
  now: providedNow,
}: {
  /**
   * When provided, the panel renders these rows without calling the
   * Tauri bridge. Used by the component test harness to drive each
   * Lean witness state without a real backend.
   */
  initialBackends?: BackendHealth[];
  now?: Date;
} = {}) {
  const live = useBackendHealth();
  const backends = initialBackends ?? live.backends;
  const error = initialBackends ? null : live.error;
  const loading = initialBackends ? false : live.loading;

  const now = providedNow ?? live.now;

  return (
    <section className="backend-health" aria-labelledby="backend-health-title">
      <header className="backend-health__header">
        <div>
          <h2 id="backend-health-title" className="backend-health__title">
            Backend health
          </h2>
          <p className="backend-health__subtitle">
            Inference backends registered to this deployment, with admission
            policy and recent call outcomes.
          </p>
        </div>
        {backends ? <FleetSummary backends={backends} /> : null}
      </header>

      {loading && !backends ? (
        <div className="backend-health__loading">Loading…</div>
      ) : null}

      {error ? (
        <div className="backend-health__error" role="alert">
          Failed to load backend health: {error}
        </div>
      ) : null}

      {backends && backends.length === 0 ? <EmptyFleet /> : null}

      {backends && backends.length > 0 ? (
        <ul className="backend-health__list">
          {backends.map((b) => (
            <BackendHealthRow key={b.backendId} backend={b} now={now} />
          ))}
        </ul>
      ) : null}
    </section>
  );
}
