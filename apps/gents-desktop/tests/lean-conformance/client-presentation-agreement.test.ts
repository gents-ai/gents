import { spawn } from "node:child_process";
import path from "node:path";
import { describe, expect, it } from "vitest";

import {
  projectDeploymentTransportStatus,
  projectDeploymentOperationalState,
  projectSyncOperationalStatus,
  projectRouteOperationalStatus,
} from "@source-inc/gents-desktop-client";

import { deployment } from "../config-panel-wiring/fixtures";

import type { Shell } from "../../src/ui/hooks/useShell";
import { presentedComposerSendStatus } from "../../src/ui/screens/SessionScreen";

type GeneratedPresentationCase = {
  name: string;
  draft_non_empty: boolean;
  canonical_reason: string | null;
  expected_kind: "ready" | "disabled";
  expected_reason: string | null;
};

type GeneratedRecoveryCase = {
  name: string;
  surface:
    "transport" | "route" | "offline" | "failed" | "incompatible" | "deploymentError";
  connected: boolean;
  routeReady: boolean;
  pairingPending: boolean;
  expected_kind: "ready" | "waiting" | "blocked";
  expected_action: null;
};

type GeneratedCases = {
  presentationCases: GeneratedPresentationCase[];
  recoveryCases: GeneratedRecoveryCase[];
};

function runLean(proofsDir: string, args: string[]): Promise<string> {
  return new Promise((resolve, reject) => {
    const child = spawn("lake", args, { cwd: proofsDir });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => (stdout += chunk));
    child.stderr.on("data", (chunk: string) => (stderr += chunk));
    child.on("error", reject);
    child.on("close", (status) => {
      if (status === 0) resolve(stdout);
      else reject(new Error(`lake ${args.join(" ")} failed\n${stdout}\n${stderr}`));
    });
  });
}

async function generatedPresentationCases(): Promise<GeneratedCases> {
  const proofsDir = path.resolve(process.cwd(), "../../crates/gents/proofs");
  await runLean(proofsDir, ["build", "Proofs.Conformance.ClientPresentationAgreement"]);
  const stdout = await runLean(proofsDir, [
    "env",
    "lean",
    "--run",
    "Proofs/Conformance/ClientPresentationAgreement.lean",
  ]);
  return JSON.parse(stdout.trim()) as GeneratedCases;
}

describe("ClientShell presentation agreement", () => {
  it("matches Lean-generated draft and automatic recovery cases", async () => {
    const { presentationCases: cases, recoveryCases } =
      await generatedPresentationCases();
    expect(cases).toHaveLength(20);
    expect(recoveryCases).toHaveLength(10);
    for (const contractCase of recoveryCases) {
      const actual =
        contractCase.surface === "transport"
          ? projectDeploymentTransportStatus(contractCase.connected)
          : contractCase.surface === "route"
            ? projectRouteOperationalStatus(
                contractCase.routeReady,
                contractCase.pairingPending,
              )
            : contractCase.surface === "deploymentError"
              ? projectDeploymentOperationalState({
                  ...deployment,
                  dialSucceeded: false,
                  lastError: "Denied",
                }).summary
              : projectSyncOperationalStatus({
                  state: contractCase.surface,
                } as Parameters<typeof projectSyncOperationalStatus>[0]);
      expect(actual.kind, contractCase.name).toBe(contractCase.expected_kind);
      expect(actual.action, contractCase.name).toBe(contractCase.expected_action);
    }
    for (const contractCase of cases) {
      const canonical: Shell["nonEmptyContentSendStatus"] =
        contractCase.canonical_reason
          ? {
              kind: "disabled",
              reason: contractCase.canonical_reason as never,
              hint: `canonical:${contractCase.canonical_reason}`,
            }
          : { kind: "ready" };
      const actual = presentedComposerSendStatus(
        contractCase.draft_non_empty ? "message" : "",
        canonical,
      );
      expect(actual.kind, contractCase.name).toBe(contractCase.expected_kind);
      expect(actual.kind === "disabled" ? actual.reason : null, contractCase.name).toBe(
        contractCase.expected_reason,
      );
      if (contractCase.draft_non_empty && canonical.kind === "disabled") {
        expect(actual, contractCase.name).toBe(canonical);
      }
    }
  }, 300_000);
});
