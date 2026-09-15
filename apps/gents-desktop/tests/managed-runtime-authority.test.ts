import { describe, expect, it } from "vitest";
import {
  authoritiesEqual,
  authorityForSelection,
  authoritySummary,
} from "../src/ui/lib/managedRuntimeAuthority";

describe("managed runtime authority selection", () => {
  const home = "/Users/A Person";
  it.each(["readwrite", "readonly"] as const)(
    "keeps root independent of %s ceiling",
    (toolCeiling) => {
      for (const toolRoot of [home, "/tmp/a folder"]) {
        expect(authorityForSelection(toolCeiling, toolRoot)).toEqual({
          toolCeiling,
          toolRoot,
        });
      }
      expect(authorityForSelection(toolCeiling, null)).toBeNull();
    },
  );
  it("omits the host root for metatools only", () => {
    expect(authorityForSelection("meta-only", home)).toEqual({
      toolCeiling: "meta-only",
      toolRoot: null,
    });
  });
  it("compares exact authority and explains unrestricted commands", () => {
    expect(
      authoritiesEqual(
        { toolCeiling: "readwrite", toolRoot: home },
        { toolCeiling: "readwrite", toolRoot: home },
      ),
    ).toBe(true);
    expect(authoritySummary({ toolCeiling: "readwrite", toolRoot: home })).toContain(
      "operating-system privacy controls",
    );
  });
});
