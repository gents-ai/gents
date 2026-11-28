import { describe, expect, it } from "vitest";

import { acceptsLiveCursor } from "@source-inc/gents-desktop-client";
import { acceptsAsyncResult } from "../../src/hooks/observationOrdering";
import {
  projectStartupPhaseAfterSettingsAnswer,
  projectStartupPhaseAfterSettingsTimeout,
  projectStartupPhaseAfterSnapshot,
  type DesktopStartupPhase,
  type SettingsAnswer,
} from "../../src/lib/loadingStatus";
import { projectFirstRunPhase, type FirstRunPhase } from "../../src/ui/lib/firstRun";
import { projectStartupGate, type StartupGate } from "../../src/lib/startupGate";
import type { ManagedServerStatus } from "@source-inc/gents-desktop-client";
import { loadLeanFamilyCases, runLeanGeneratorJson } from "./leanGenerator";

type ObservationFenceCase = {
  current: number;
  captured: number;
  accepted: boolean;
};

describe("generated client observation ordering", () => {
  it("drives the production async-result fence with every Lean row", async () => {
    const {
      fences: cases,
      startup,
      firstRun,
      liveDeltas,
    } = await runLeanGeneratorJson<{
      liveDeltas: {
        base: number | null;
        current: number | null;
        terminal: boolean;
        accepted: boolean;
      }[];
      fences: ObservationFenceCase[];
      startup: {
        phase: DesktopStartupPhase;
        running: boolean;
        autostartDeclined: boolean;
        expected: DesktopStartupPhase;
      }[];
      firstRun: {
        phase: FirstRunPhase;
        snapshotReady: boolean;
        initConfigStatus: "absent" | "configured" | "unreadable" | "unknown";
        localRuntimeAllowed: boolean;
        remoteConfigured: boolean;
        localHomeLost: boolean;
        needsSetup: boolean;
        authoritative: boolean;
        expected: FirstRunPhase;
      }[];
    }>(
      "Proofs.Conformance.ClientObservationOrdering:olean",
      "Proofs/Conformance/ClientObservationOrdering.lean",
    );

    expect(cases).toHaveLength(25);
    expect(liveDeltas).toHaveLength(18);
    for (const row of liveDeltas) {
      expect(
        acceptsLiveCursor(row.base?.toString(), row.current?.toString(), row.terminal),
      ).toBe(row.accepted);
    }
    for (const row of cases) {
      expect(acceptsAsyncResult(row.current, row.captured)).toBe(row.accepted);
    }
    expect(startup).toHaveLength(28);
    for (const row of startup) {
      expect(
        projectStartupPhaseAfterSnapshot(row.phase, row.running, row.autostartDeclined),
      ).toBe(row.expected);
    }
    expect(firstRun).toHaveLength(768);
    for (const row of firstRun) {
      expect(
        projectFirstRunPhase(
          row.phase,
          row.snapshotReady,
          row.initConfigStatus === "unknown" ? null : row.initConfigStatus,
          row.needsSetup,
          row.authoritative,
          row.localRuntimeAllowed,
          row.remoteConfigured,
          row.localHomeLost,
        ),
      ).toBe(row.expected);
    }
  }, 300_000);
});

type StartupGateCase = {
  input: {
    settled: boolean;
    init_config_status: "absent" | "configured" | "unreadable" | "unknown";
    node_not_connected: boolean;
  };
  expected: StartupGate;
};

type LeanStartupPhase =
  | "checking_managed_server"
  | "loading_configuration"
  | "starting_client"
  | "managed_server_error"
  | "configuration_error"
  | "client_error"
  | "ready";

type SettingsAnsweredCase = {
  input: {
    phase: LeanStartupPhase;
    current: boolean;
    answer: SettingsAnswer;
    running: boolean;
    autostart_declined: boolean;
  };
  expected: LeanStartupPhase;
};

type SettingsReadTimedOutCase = {
  input: { phase: LeanStartupPhase; current: boolean };
  expected: LeanStartupPhase;
};

const startupPhase = (phase: LeanStartupPhase) =>
  phase.replaceAll("_", "-") as DesktopStartupPhase;

describe("generated startup gate and settings read", () => {
  it("drives the startup gate and the settings read's answers with every case of their Lean families", async () => {
    const gate = await loadLeanFamilyCases<StartupGateCase>(
      "families::client_snapshot_observation_startup_gate::cases()",
    );
    expect(gate).toHaveLength(16);
    /* a node the connect screen calls not connected is a stopped one this
       desktop is not enrolled with; otherwise there is no local node, or an
       enrolled one, each in turn */
    const stopped = { state: "stopped", pairingReady: false } as ManagedServerStatus;
    const enrolled = { state: "running", pairingReady: true } as ManagedServerStatus;
    let other = 0;
    for (const { input, expected } of gate) {
      const localNode = input.node_not_connected
        ? stopped
        : other++ % 2
          ? enrolled
          : null;
      expect(
        projectStartupGate(
          input.settled,
          input.init_config_status === "unknown" ? null : input.init_config_status,
          localNode,
        ),
      ).toBe(expected);
    }

    const answered = await loadLeanFamilyCases<SettingsAnsweredCase>(
      "families::client_snapshot_observation_settings_answered::cases()",
    );
    expect(answered).toHaveLength(112);
    for (const { input, expected } of answered) {
      expect(
        projectStartupPhaseAfterSettingsAnswer(
          startupPhase(input.phase),
          input.current,
          input.answer,
          input.running,
          input.autostart_declined,
        ),
        JSON.stringify(input),
      ).toBe(startupPhase(expected));
    }

    const timedOut = await loadLeanFamilyCases<SettingsReadTimedOutCase>(
      "families::client_snapshot_observation_settings_read_timed_out::cases()",
    );
    expect(timedOut).toHaveLength(14);
    for (const { input, expected } of timedOut) {
      expect(
        projectStartupPhaseAfterSettingsTimeout(
          startupPhase(input.phase),
          input.current,
        ),
        JSON.stringify(input),
      ).toBe(startupPhase(expected));
    }
  }, 300_000);
});
