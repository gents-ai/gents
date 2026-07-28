import { describe, expect, it } from "vitest";
import type {
  BackgroundedToolView,
  NativeExecutorStatusView,
} from "@source-inc/gents-desktop-client";

import {
  STUCK_DWELL_MS,
  correlateProcess,
  derivedState,
  formatAge,
} from "./derivedState.js";

const baseRow: BackgroundedToolView = {
  requestId: "req_a17",
  toolCallId: "tc_001",
  toolName: "grep",
  lifecycleState: "running",
  status: null,
  startedAt: new Date(Date.now() - 5_000).toISOString(),
  ageMs: 5_000,
  deadlineAt: new Date(Date.now() + 60_000).toISOString(),
  deadlineExpired: false,
  awaitMode: "background",
  cancelPolicy: "cascade",
  childRequestId: null,
  stuckSince: null,
  cancelPendingRemoteAck: false,
  nativeExecutor: null,
};

describe("derivedState", () => {
  it("returns 'background' for a healthy bg row with lifecycle_state=running", () => {
    expect(derivedState(baseRow, Date.now())).toBe("background");
  });

  it("returns 'background' when await_mode=background and lifecycle_state is unset", () => {
    expect(derivedState({ ...baseRow, lifecycleState: null }, Date.now())).toBe(
      "background",
    );
  });

  it("returns 'deadline+' when deadline_expired flag is set", () => {
    expect(
      derivedState({ ...baseRow, deadlineExpired: true }, Date.now()),
    ).toBe("deadline+");
  });

  it("returns 'cancelPending' when cancel_pending_remote_ack is true", () => {
    expect(
      derivedState({ ...baseRow, cancelPendingRemoteAck: true }, Date.now()),
    ).toBe("cancelPending");
  });

  it("returns 'stuck' once dwell >= STUCK_DWELL_MS", () => {
    const stuckSince = new Date(
      Date.now() - STUCK_DWELL_MS - 100,
    ).toISOString();
    expect(derivedState({ ...baseRow, stuckSince }, Date.now())).toBe("stuck");
  });

  it("does not return 'stuck' for stuck_since within the dwell window", () => {
    const stuckSince = new Date(Date.now() - 1_000).toISOString();
    expect(derivedState({ ...baseRow, stuckSince }, Date.now())).toBe(
      "background",
    );
  });

  it("prefers 'stuck' over 'cancelPending' when both apply", () => {
    const stuckSince = new Date(
      Date.now() - STUCK_DWELL_MS - 100,
    ).toISOString();
    expect(
      derivedState(
        { ...baseRow, stuckSince, cancelPendingRemoteAck: true },
        Date.now(),
      ),
    ).toBe("stuck");
  });
});

describe("correlateProcess", () => {
  const ne = (
    overrides: Partial<NativeExecutorStatusView>,
  ): NativeExecutorStatusView => ({
    id: 901,
    pid: 41812,
    argv0: "/usr/bin/grep",
    toolName: "grep",
    startedAt: new Date().toISOString(),
    ageMs: 5_000,
    ...overrides,
  });

  it("returns pid label when a single native executor matches (tool_name, started_at ±1s)", () => {
    const row = { ...baseRow, startedAt: new Date(1_000_000).toISOString() };
    const exec = ne({ startedAt: new Date(1_000_500).toISOString() });
    expect(correlateProcess(row, [exec]).label).toBe("pid 41812");
  });

  it("returns 'native <id>' when multiple executors match within the window", () => {
    const row = { ...baseRow, startedAt: new Date(1_000_000).toISOString() };
    const e1 = ne({ id: 901, startedAt: new Date(999_900).toISOString() });
    const e2 = ne({ id: 902, startedAt: new Date(1_000_500).toISOString() });
    const result = correlateProcess(row, [e1, e2]);
    expect(result.label).toMatch(/^native /);
    expect(result.tooltip).toContain("ambiguous");
  });

  it("falls back to 'child req_<id>' when child_request_id is set and no native executor", () => {
    expect(
      correlateProcess({ ...baseRow, childRequestId: "req_b91" }, []).label,
    ).toBe("child req_b91");
  });

  it("falls back to '—' when no executor and no child request", () => {
    expect(correlateProcess(baseRow, []).label).toBe("—");
  });
});

describe("formatAge", () => {
  it("renders MM:SS for under one hour", () => {
    expect(formatAge(48_000)).toBe("00:48");
    expect(formatAge(125_000)).toBe("02:05");
  });
  it("renders HH:MM:SS for one hour or more", () => {
    expect(formatAge(3_600_000)).toBe("01:00:00");
    expect(formatAge(3_725_000)).toBe("01:02:05");
  });
  it("clamps negatives to 00:00", () => {
    expect(formatAge(-1_000)).toBe("00:00");
  });
});
