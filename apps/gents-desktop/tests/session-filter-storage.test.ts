import { renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useSessionFilter } from "../src/ui/screens/SessionFilters";

describe("a saved session filter", () => {
  afterEach(() => vi.unstubAllGlobals());

  /* an earlier build offered "Needs you"; its saved pick must not be read
     as another state */
  it("drops a state this build no longer offers", () => {
    const saved = JSON.stringify({
      states: ["held", "live"],
      sources: [],
      behaviors: [],
    });
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => (key === "gents-session-filter" ? saved : null),
      setItem: vi.fn(),
      removeItem: vi.fn(),
    });
    const { result } = renderHook(() => useSessionFilter());
    expect(result.current[0].states).toEqual(["live"]);
  });
});
