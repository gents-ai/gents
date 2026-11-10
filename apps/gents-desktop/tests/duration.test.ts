import { describe, expect, it } from "vitest";
import { duration } from "@/lib/duration";

describe("a span as a timer reads it", () => {
  it("gives seconds, then minutes, hours and days with the unit below", () => {
    expect(duration(3_000)).toBe("3s");
    expect(duration(42_900)).toBe("42s");
    expect(duration(4 * 60_000 + 12_000)).toBe("4m 12s");
    expect(duration(63 * 60_000 + 40_000)).toBe("1h 3m");
    expect(duration(28 * 3_600_000 + 59 * 60_000)).toBe("1d 4h");
  });

  it("leaves out a zero lower unit", () => {
    expect(duration(5 * 60_000)).toBe("5m");
    expect(duration(2 * 3_600_000 + 30_000)).toBe("2h");
    expect(duration(48 * 3_600_000 + 59 * 60_000)).toBe("2d");
  });

  it("never rounds up past what passed", () => {
    expect(duration(59_999)).toBe("59s");
    expect(duration(400)).toBe("<1s");
    expect(duration(0)).toBe("<1s");
    expect(duration(-5_000)).toBe("<1s");
  });
});
