import { describe, expect, it } from "vitest";

import { keepsSeen, sessionShownOn } from "../../src/hooks/sessionShown";
import { loadLeanFamilyCases } from "./leanGenerator";

type Input = {
  visible: boolean;
  wide: boolean;
  page: "session" | "other";
  listed: boolean;
};

describe("generated session shown", () => {
  it("drives sessionShownOn and keepsSeen with every Lean case", async () => {
    const cases = await loadLeanFamilyCases<{ input: Input; expected: boolean }>(
      "families::session_shown_contracts_seen::cases()",
    );
    expect(cases).toHaveLength(16);
    for (const { input, expected } of cases) {
      /* the session's own page, or the sessions list as any other page */
      const shown = sessionShownOn(
        input.wide,
        input.page === "session" ? "session" : "sessions",
      );
      expect(keepsSeen(input.visible, shown, input.listed), JSON.stringify(input)).toBe(
        expected,
      );
    }
  }, 600_000);
});
