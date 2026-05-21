import { describe, expect, it } from "vitest";
import { render, screen, fireEvent, within } from "@testing-library/react";

import { BackendHealthPanel } from "../src/components/backendHealth";
import {
  deriveDisplayState,
  STATE_LABEL,
} from "../src/components/backendHealth/displayState";
import type {
  BackendDisplayState,
  BackendHealth,
} from "../src/components/backendHealth/types";

/**
 * One row per Lean BackendHealthAdmissionCase
 * (`crates/defra-agent/proofs/Proofs/Conformance/ContractCases/BoundaryRuntime.lean:214`).
 * The Rust-side consumer test
 * (`backend_registry::tests::display_state_matches_every_lean_backend_health_admission_case`)
 * drives the same inputs through `derive_display_state`; this enumeration
 * checks the JS `deriveDisplayState` agrees and that each bucket renders a
 * distinct label / data-state attribute in the panel.
 */
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

    // Fleet summary chip count matches the variety of buckets present.
    // `disabled` is the only non-`available` case where `enabled=false`,
    // so the summary surfaces every distinct state plus one neutral
    // "N registered" chip.
    const distinctStates = new Set(backends.map((b) => b.displayState));
    const expectedChipCount = distinctStates.size + 1; // +1 for "N registered"
    const chips = screen
      .getAllByText(
        /registered|available|unhealthy|stale|rate-limited|circuit-open|unknown|disabled/,
      )
      .filter((el) => el.className.includes("backend-health__summary-chip"));
    expect(chips.length).toBe(expectedChipCount);

    // Every backend renders a row with the right data-state and label.
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

    // The seven buckets must be pairwise distinct in the DOM. If any two
    // collapse to the same data-state we'd lose operator-visible
    // information that the Lean witnesses model.
    const renderedStates = LEAN_WITNESSES.map((w) => w.expectedDisplayState);
    expect(new Set(renderedStates).size).toBe(renderedStates.length);
  });

  it("expanding a row reveals admission policy + models + recent calls sections", () => {
    const withCalls: BackendHealth = {
      ...makeBackend(LEAN_WITNESSES[0]),
      recentCalls: [
        {
          callId: "c-1",
          callSeq: 42,
          callKind: "completion",
          callState: "completed",
          failureReason: null,
          queuedAt: "2026-05-20T17:30:00Z",
          startedAt: "2026-05-20T17:30:01Z",
          endedAt: "2026-05-20T17:30:05Z",
          queueDepthAtEnqueue: 0,
          promptTokens: 100,
          completionTokens: 25,
        },
      ],
    };
    render(<BackendHealthPanel initialBackends={[withCalls]} now={NOW} />);

    const summaryButton = screen.getByRole("button", {
      name: /available fixture/i,
    });
    expect(summaryButton.getAttribute("aria-expanded")).toBe("false");

    fireEvent.click(summaryButton);

    expect(summaryButton.getAttribute("aria-expanded")).toBe("true");
    expect(
      screen.getByRole("heading", { name: /Admission policy & probe/i }),
    ).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: /^Models$/i })).toBeInTheDocument();
    expect(
      screen.getByRole("heading", {
        name: /Recent calls \(InferenceCall, last 10\)/i,
      }),
    ).toBeInTheDocument();
  });

  it("renders the empty-fleet card when zero backends are returned", () => {
    render(<BackendHealthPanel initialBackends={[]} now={NOW} />);
    expect(screen.getByText(/No backends registered/i)).toBeInTheDocument();
    expect(screen.queryByText(/available fixture/i)).not.toBeInTheDocument();
  });

  it("rate-limited and circuit-open rows surface their failure_reason hint", () => {
    const rateLimited: BackendHealth = {
      ...makeBackend(LEAN_WITNESSES[5]),
      recentCalls: [
        {
          callId: "rl-1",
          callSeq: 1224,
          callKind: "completion",
          callState: "failed",
          failureReason: "upstream 429",
          queuedAt: "2026-05-20T17:32:00Z",
          startedAt: null,
          endedAt: "2026-05-20T17:32:01Z",
          queueDepthAtEnqueue: 12,
          promptTokens: null,
          completionTokens: null,
        },
      ],
    };
    const circuitOpen: BackendHealth = {
      ...makeBackend(LEAN_WITNESSES[6]),
      recentCalls: [
        {
          callId: "co-1",
          callSeq: 318,
          callKind: "completion",
          callState: "failed",
          failureReason: "BackendGone",
          queuedAt: "2026-05-20T17:32:00Z",
          startedAt: null,
          endedAt: "2026-05-20T17:32:01Z",
          queueDepthAtEnqueue: 0,
          promptTokens: null,
          completionTokens: null,
        },
      ],
    };
    render(
      <BackendHealthPanel initialBackends={[rateLimited, circuitOpen]} now={NOW} />,
    );
    expect(screen.getByText("upstream 429")).toBeInTheDocument();
    expect(screen.getByText("BackendGone")).toBeInTheDocument();
  });
});
