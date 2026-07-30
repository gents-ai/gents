import { describe, expect, it } from "vitest";
import { render, screen, within } from "@testing-library/react";

import { BackendHealthPanel } from "@source-inc/gents-desktop-operations";
import { deriveDisplayState, STATE_LABEL } from "@source-inc/gents-desktop-operations";
import type {
  BackendDisplayState,
  BackendHealth,
} from "@source-inc/gents-desktop-client";

type WitnessCase = {
  name: string;
  enabled: boolean;
  probeStatus: string;
  expectedDisplayState: BackendDisplayState;
  expectedAvailable: boolean;
};

const LEAN_WITNESSES: WitnessCase[] = [
  {
    name: "enabled_healthy_backend_is_available_from_observed_document",
    enabled: true,
    probeStatus: "healthy",
    expectedDisplayState: "available",
    expectedAvailable: true,
  },
  {
    name: "disabled_healthy_backend_is_unavailable_from_observed_document",
    enabled: false,
    probeStatus: "healthy",
    expectedDisplayState: "disabled",
    expectedAvailable: false,
  },
  {
    name: "enabled_unhealthy_backend_is_unavailable_from_observed_document",
    enabled: true,
    probeStatus: "unhealthy",
    expectedDisplayState: "unhealthy",
    expectedAvailable: false,
  },
  {
    name: "enabled_unknown_backend_is_unavailable_from_observed_document",
    enabled: true,
    probeStatus: "unknown",
    expectedDisplayState: "unknown",
    expectedAvailable: false,
  },
  {
    name: "enabled_stale_backend_is_unavailable_from_observed_document",
    enabled: true,
    probeStatus: "stale",
    expectedDisplayState: "stale",
    expectedAvailable: false,
  },
  {
    name: "enabled_rate_limited_backend_is_unavailable_from_observed_document",
    enabled: true,
    probeStatus: "rate_limited",
    expectedDisplayState: "rate-limited",
    expectedAvailable: false,
  },
  {
    name: "enabled_circuit_open_backend_is_unavailable_from_observed_document",
    enabled: true,
    probeStatus: "circuit_open",
    expectedDisplayState: "circuit-open",
    expectedAvailable: false,
  },
];

function makeBackend(witness: WitnessCase): BackendHealth {
  return {
    backendId: `id-${witness.expectedDisplayState}`,
    name: `${witness.expectedDisplayState} fixture`,
    providerKind: "OpenAiCompatible",
    endpoint: `https://example.test/${witness.expectedDisplayState}/v1`,
    enabled: witness.enabled,
    probeStatus: witness.probeStatus,
    displayState: deriveDisplayState(witness.enabled, witness.probeStatus),
    lastProbe: "2026-05-20T17:30:00Z",
    maxConcurrent: 4,
    maxQueueDepth: 50,
    models: ["fixture-model"],
    recentCalls: [],
  };
}

const NOW = new Date("2026-05-20T17:32:18Z");

describe("BackendHealthPanel — Lean witness coverage", () => {
  it("deriveDisplayState lands every Lean witness in the expected bucket", () => {
    for (const w of LEAN_WITNESSES) {
      const got = deriveDisplayState(w.enabled, w.probeStatus);
      expect(got, `case ${w.name}`).toBe(w.expectedDisplayState);
      const isAvailable = got === "available";
      expect(isAvailable, `case ${w.name} availability`).toBe(w.expectedAvailable);
    }
  });

  it("renders all seven Lean witnesses with visually distinct state badges", () => {
    const backends = LEAN_WITNESSES.map(makeBackend);
    render(<BackendHealthPanel initialBackends={backends} now={NOW} />);

    const distinctStates = new Set(backends.map((b) => b.displayState));
    const expectedChipCount = distinctStates.size + 1;
    const chips = screen
      .getAllByText(
        /registered|available|unhealthy|stale|rate-limited|circuit-open|unknown|disabled/,
      )
      .filter((el) => el.className.includes("backend-health__summary-chip"));
    expect(chips.length).toBe(expectedChipCount);

    for (const w of LEAN_WITNESSES) {
      const row = screen
        .getByText(`${w.expectedDisplayState} fixture`)
        .closest("li.backend-health__row");
      expect(row, `row for ${w.name}`).not.toBeNull();
      expect(row?.getAttribute("data-state"), `data-state for ${w.name}`).toBe(
        w.expectedDisplayState,
      );
      const stateLabel = within(row as HTMLElement).getByText(
        STATE_LABEL[w.expectedDisplayState],
      );
      expect(stateLabel.getAttribute("data-state")).toBe(w.expectedDisplayState);
    }

    const renderedStates = LEAN_WITNESSES.map((w) => w.expectedDisplayState);
    expect(new Set(renderedStates).size).toBe(renderedStates.length);
  });
});
