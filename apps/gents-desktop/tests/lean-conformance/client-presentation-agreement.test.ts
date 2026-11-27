import { describe, expect, it } from "vitest";

import {
  projectRuntimeConnectionStatus,
  projectDeploymentOperationalState,
  projectSyncOperationalStatus,
  projectRouteOperationalStatus,
} from "@source-inc/gents-desktop-client";

import { deployment } from "../config-panel-wiring/fixtures";

import type { ChatShellProjection } from "@source-inc/gents-desktop-chat";
import { presentedComposerSendStatus } from "../../src/ui/screens/session/composerParts";
import { runLeanGeneratorJson } from "./leanGenerator";

type GeneratedPresentationCase = {
  name: string;
  draft_non_empty: boolean;
  canonical_kind: "ready" | "queue" | "disabled";
  canonical_reason: string | null;
  expected_kind: "ready" | "queue" | "disabled";
  expected_reason: string | null;
};

type GeneratedRecoveryCase = {
  name: string;
  surface: "transport" | "route" | "sync" | "deploymentError";
  connected: boolean;
  refusal: { cause: string; nextAction: string } | null;
  sync?: { state: string; cause: string | null; nextAction: string | null };
  expected_kind: "ready" | "waiting" | "blocked";
  expected_action: null;
};

type GeneratedCases = {
  presentationCases: GeneratedPresentationCase[];
  recoveryCases: GeneratedRecoveryCase[];
};

function generatedPresentationCases(): Promise<GeneratedCases> {
  return runLeanGeneratorJson<GeneratedCases>(
    "Proofs.Conformance.ClientPresentationAgreement:olean",
    "Proofs/Conformance/ClientPresentationAgreement.lean",
  );
}

describe("ClientShell presentation agreement", () => {
  it("matches Lean-generated draft and automatic recovery cases", async () => {
    const { presentationCases: cases, recoveryCases } =
      await generatedPresentationCases();
    expect(cases).toHaveLength(22);
    expect(recoveryCases).toHaveLength(11);
    for (const contractCase of recoveryCases) {
      const actual =
        contractCase.surface === "transport"
          ? projectRuntimeConnectionStatus(
              contractCase.connected ? "connected" : "disconnected",
            )
          : contractCase.surface === "route"
            ? projectRouteOperationalStatus(
                contractCase.refusal && {
                  ...contractCase.refusal,
                  detail: contractCase.refusal.cause,
                },
              )
            : contractCase.surface === "deploymentError"
              ? projectDeploymentOperationalState({
                  ...deployment,

                  runtime: {
                    reconcilePhase: null,
                    lastReconcileResult: null,
                    lastReconcileError: "Denied",
                    updatedAt: null,
                    agentExecutorCapacity: null,
                    agentExecutorQueueDepth: null,
                  },
                }).summary
              : projectSyncOperationalStatus({
                  ...contractCase.sync!,

                  runtimeCount: 1,
                  connectedRuntimeCount: 0,
                });
      expect(actual.kind, contractCase.name).toBe(contractCase.expected_kind);
      expect(actual.action, contractCase.name).toBe(contractCase.expected_action);
    }
    for (const contractCase of cases) {
      const canonical: ChatShellProjection["nonEmptyContentSendStatus"] =
        contractCase.canonical_kind === "disabled"
          ? {
              kind: "disabled",
              reason: contractCase.canonical_reason as never,
              hint: `canonical:${contractCase.canonical_reason}`,
            }
          : contractCase.canonical_kind === "queue"
            ? { kind: "queue", turnState: "running", hint: "canonical:queue" }
            : { kind: "ready" };
      const actual = presentedComposerSendStatus(
        contractCase.draft_non_empty ? "message" : "",
        canonical,
      );
      expect(actual.kind, contractCase.name).toBe(contractCase.expected_kind);
      expect(actual.kind === "disabled" ? actual.reason : null, contractCase.name).toBe(
        contractCase.expected_reason,
      );
      if (contractCase.draft_non_empty && canonical.kind !== "ready") {
        expect(actual, contractCase.name).toBe(canonical);
      }
    }
  }, 300_000);
});
