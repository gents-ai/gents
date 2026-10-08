import { afterEach, describe, expect, it, vi } from "vitest";

import { registry } from "@/contrib/registry";
import type { Contribution } from "@/contrib/types";

const AREA = "test.area";
const OTHER = "test.other";
const disposers: Array<() => void> = [];
afterEach(() => {
  disposers.splice(0).forEach((dispose) => dispose());
});

const make = (id: string, extra: Partial<Contribution> = {}): Contribution => ({
  id,
  area: AREA,
  ...extra,
});

describe("contribution registry", () => {
  it("sorts by order, then registration, and is referentially stable until mutated", () => {
    disposers.push(
      registry.registerMany([make("b", { order: 2 }), make("a", { order: 1 })]),
    );
    disposers.push(registry.register(make("c", { order: 1 })));
    const first = registry.getArea(AREA);
    expect(first.map((c) => c.id)).toEqual(["a", "c", "b"]);
    expect(registry.getArea(AREA)).toBe(first);
    disposers.push(registry.register(make("z", { order: 0 })));
    expect(registry.getArea(AREA)).not.toBe(first);
    expect(registry.getArea(AREA)[0]!.id).toBe("z");
  });

  it("invalidates only the area that mutated", () => {
    const onArea = vi.fn();
    const onOther = vi.fn();
    const onAny = vi.fn();
    disposers.push(registry.subscribeArea(AREA, onArea));
    disposers.push(registry.subscribeArea(OTHER, onOther));
    disposers.push(registry.subscribe(onAny));
    const otherBefore = registry.getArea(OTHER);
    disposers.push(registry.register(make("x")));
    expect(onArea).toHaveBeenCalledTimes(1);
    expect(onOther).not.toHaveBeenCalled();
    expect(onAny).toHaveBeenCalledTimes(1);
    expect(registry.getArea(OTHER)).toBe(otherBefore);
  });

  it("notifies once per batch, however many entries it carries", () => {
    const onArea = vi.fn();
    disposers.push(registry.subscribeArea(AREA, onArea));
    disposers.push(registry.registerMany([make("p"), make("q"), make("r")]));
    expect(onArea).toHaveBeenCalledTimes(1);
  });

  it("drops disabled and when()-false entries from the snapshot", () => {
    disposers.push(
      registry.registerMany([
        make("on"),
        make("off", { enabled: false }),
        make("later", { when: () => false }),
      ]),
    );
    expect(registry.getArea(AREA).map((c) => c.id)).toEqual(["on"]);
  });

  it("replaces on re-register, and a stale disposer leaves the replacement alone", () => {
    const first = make("same", { title: "one" });
    const second = make("same", { title: "two" });
    const dispose1 = registry.register(first);
    const dispose2 = registry.register(second);
    disposers.push(dispose2);
    expect(registry.getArea(AREA).map((c) => c.title)).toEqual(["two"]);
    dispose1();
    expect(registry.getArea(AREA).map((c) => c.title)).toEqual(["two"]);
    dispose2();
    expect(registry.getArea(AREA)).toHaveLength(0);
  });

  it("returns the same frozen empty list for an area nobody registered into", () => {
    expect(registry.getArea("nobody")).toBe(registry.getArea("nobody"));
    expect(registry.getArea("nobody")).toHaveLength(0);
  });
});
