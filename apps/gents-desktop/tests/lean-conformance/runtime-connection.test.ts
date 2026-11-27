import { describe, expect, it } from "vitest";
import {
  projectRuntimeConnectionStatus,
  runtimeConnectionBlocksAdmission,
} from "@source-inc/gents-desktop-client";
import { runLeanGeneratorJson } from "./leanGenerator";

describe("runtime connection admission", () => {
  it("matches the Lean observation owner", async () => {
    const contract = await runLeanGeneratorJson<{
      cases: { status: string; blocksAdmission: boolean }[];
      presentations: { status: string | null; kind: string; reason: string }[];
    }>(
      "Proofs.Conformance.RuntimeConnectionObservation:olean",
      "Proofs/Conformance/RuntimeConnectionObservation.lean",
    );
    expect(contract.cases.length).toBeGreaterThan(0);
    for (const row of contract.cases) {
      expect(runtimeConnectionBlocksAdmission(row.status)).toBe(row.blocksAdmission);
    }
    expect(contract.presentations.length).toBeGreaterThan(0);
    const labels = new Map<string, string>();
    for (const row of contract.presentations) {
      const shown = projectRuntimeConnectionStatus(row.status);
      expect({ status: row.status, kind: shown.kind, reason: shown.reason }).toEqual(
        row,
      );
      const previous = labels.get(shown.label);
      expect(previous === undefined || previous === row.reason).toBe(true);
      labels.set(shown.label, row.reason);
    }
  }, 300_000);
});
