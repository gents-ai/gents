import { act, render } from "@testing-library/react";
import { MemoryNavProvider, useNav, type Nav } from "@gents/shell";
import { describe, expect, it } from "vitest";
import { bindNav, leave, navigate, replace } from "../src/ui/lib/router";

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

describe("leaving a draft", () => {
  const list = { name: "agent" as const, nodeDid: "did:a", section: "agents" };
  const draft = { ...list, item: "~new" };
  function mount() {
    let nav: Nav | null = null;
    function Bind() {
      nav = useNav();
      bindNav(nav);
      return null;
    }
    render(
      <MemoryNavProvider initial={list}>
        <Bind />
      </MemoryNavProvider>,
    );
    act(() => navigate(draft));
    return () => nav!;
  }

  it("goes back to the page that opened it, with nothing left to go back to", () => {
    const nav = mount();
    act(() => leave(draft, list));
    expect(nav().route).toEqual(list);
    expect(nav().canBack).toBe(false);
  });

  it("puts the saved document in the draft's place", () => {
    const nav = mount();
    act(() => replace(draft, { ...list, item: "reviewer" }));
    expect(nav().route).toEqual({ ...list, item: "reviewer" });
    act(() => nav().back());
    expect(nav().route).toEqual(list);
    expect(nav().canBack).toBe(false);
  });

  it("stays where the person went when the save settles after they left", () => {
    const nav = mount();
    const tools = { ...list, section: "tools" };
    act(() => navigate(tools));
    act(() => replace(draft, { ...list, item: "reviewer" }));
    act(() => leave(draft, list));
    expect(nav().route).toEqual(tools);
  });
});
