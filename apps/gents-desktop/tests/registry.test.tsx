import { act, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { createRegistry } from "../src/ui/app/registry";

describe("a registry", () => {
  it("keeps registration order, replaces by id in place, and removes what it added", () => {
    const registry = createRegistry<{ id: string; v: number }>();
    registry.register({ id: "a", v: 1 });
    registry.register({ id: "b", v: 1 });
    const replaced = { id: "a", v: 2 };
    const remove = registry.register(replaced);
    expect(registry.list()).toEqual([replaced, { id: "b", v: 1 }]);
    remove();
    expect(registry.list().map((x) => x.id)).toEqual(["b"]);
  });

  it("shows a contribution registered after the area first drew", () => {
    const registry = createRegistry<{ id: string; label: string }>();
    function Area() {
      return (
        <p>
          {registry
            .useList()
            .map((x) => x.label)
            .join(",") || "empty"}
        </p>
      );
    }
    render(<Area />);
    expect(screen.getByText("empty")).toBeInTheDocument();
    act(() => void registry.register({ id: "late", label: "Late" }));
    expect(screen.getByText("Late")).toBeInTheDocument();
  });
});
