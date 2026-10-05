import { renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { useDivider } from "../src/ui/lib/divider";

const base = {
  key: "divider-scope-test",
  initial: 520,
  min: 320,
  paneMin: 360,
  gap: 8,
  container: 1600,
  rail: 56,
  onClosed: () => {},
};

describe("the dock divider across scopes", () => {
  it("springs open when the dock opens where it is", () => {
    const { result, rerender } = renderHook(
      (props: { open: boolean }) =>
        useDivider({ ...base, scope: "session:a", ...props }),
      {
        initialProps: { open: false },
      },
    );
    rerender({ open: true });
    expect(result.current.settling).toBe(true);
    expect(result.current.pos).toBeLessThan(520);
  });

  it("shows another session's open dock as it stands, without motion", () => {
    const { result, rerender } = renderHook(
      (props: { open: boolean; scope: string }) => useDivider({ ...base, ...props }),
      { initialProps: { open: false, scope: "session:a" } },
    );
    rerender({ open: true, scope: "session:b" });
    expect(result.current.settling).toBe(false);
    expect(result.current.pos).toBe(520);
    rerender({ open: false, scope: "session:a" });
    expect(result.current.settling).toBe(false);
    expect(result.current.pos).toBe(0);
  });
});
