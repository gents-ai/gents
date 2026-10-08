import { act, render } from "@testing-library/react";
import { MemoryNavProvider, useNav, type Nav } from "@gents/shell";
import { describe, expect, it } from "vitest";
import { bindNav, navigate } from "../src/ui/lib/router";

describe("navigate", () => {
  it("does not push the route already shown", () => {
    let nav: Nav | null = null;
    function Bind() {
      nav = useNav();
      bindNav(nav);
      return null;
    }
    render(
      <MemoryNavProvider>
        <Bind />
      </MemoryNavProvider>,
    );
    act(() => navigate({ name: "mailbox" }));
    act(() => navigate({ name: "mailbox" }));
    act(() => nav!.back());
    expect(nav!.route).toEqual({ name: "sessions" });
    expect(nav!.canBack).toBe(false);
  });
});
