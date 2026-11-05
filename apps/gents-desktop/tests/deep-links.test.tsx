import { act, fireEvent, render, renderHook } from "@testing-library/react";
import { MemoryNavProvider, useNav, type Nav } from "@gents/shell";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useOutsideLinks, type LinkSource } from "../src/ui/app/useOutsideLinks";
import { parseLink } from "../src/ui/lib/links";
import { bindNav, interceptNavClicks, openLink } from "../src/ui/lib/router";
import { Markdown } from "../src/ui/screens/Markdown";

describe("parseLink", () => {
  it("reads a session from outside the app and from inside a message", () => {
    expect(parseLink("gents://session/s-1")).toEqual({
      kind: "session",
      sessionId: "s-1",
    });
    expect(parseLink("gents:session/s-1")).toEqual({
      kind: "session",
      sessionId: "s-1",
    });
    expect(parseLink("GENTS://Session/s-1/")).toEqual({
      kind: "session",
      sessionId: "s-1",
    });
  });

  it("decodes the id and reads the node it names", () => {
    expect(parseLink("gents://session/a%20b?node=did%3Akey%3Az6Mk")).toEqual({
      kind: "session",
      sessionId: "a b",
      nodeDid: "did:key:z6Mk",
    });
  });

  it("opens nothing it doesn't know, messages included", () => {
    for (const href of [
      "gents://session/",
      "gents://session/s-1/message/4",
      "gents://pack/acme/game",
      "gents://node/add?address=10.0.0.1:9191",
      "gents:session",
      "gents://session/%E0%A4%A",
      "https://example.com/session/s-1",
    ]) {
      expect(parseLink(href), href).toBeNull();
    }
  });
});

function mountNav() {
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
  return () => nav!;
}

describe("following a gents link", () => {
  afterEach(() => bindNav(null));

  it("goes to the session, leaving its node to be found when the link names none", () => {
    const nav = mountNav();
    act(() => void openLink("gents://session/s-1"));
    expect(nav().route).toEqual({ name: "session", sessionId: "s-1" });
    act(() => void openLink("gents://session/s-2?node=did:key:z6Mk"));
    expect(nav().route).toEqual({
      name: "session",
      sessionId: "s-2",
      nodeDid: "did:key:z6Mk",
    });
  });

  it("stays put on a link it can't open", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const nav = mountNav();
    let opened = true;
    act(() => void (opened = openLink("gents://pack/acme/game")));
    expect(opened).toBe(false);
    expect(nav().route).toEqual({ name: "sessions" });
    warn.mockRestore();
  });

  it("follows a gents link in a reply through the router", () => {
    const nav = mountNav();
    const stop = interceptNavClicks(document);
    const { getByRole } = render(
      <Markdown>{"See [the plan](gents:session/s-9) for details."}</Markdown>,
    );
    const link = getByRole("link", { name: "the plan" });
    expect(link.getAttribute("href")).toBe("gents:session/s-9");
    act(() => void fireEvent.click(link));
    expect(nav().route).toEqual({ name: "session", sessionId: "s-9" });
    stop();
  });

  it("still drops schemes a reply has no business linking to", () => {
    const { getByText } = render(<Markdown>{"[run](javascript:alert(1))"}</Markdown>);
    expect(getByText("run").getAttribute("href")).toBe("");
  });
});

describe("links from outside the app", () => {
  function source(waiting: string[][]) {
    let arrived: (() => void) | null = null;
    const take = vi.fn(async () => waiting.shift() ?? []);
    const unlisten = vi.fn();
    const links: LinkSource = {
      take,
      onArrived: async (handler) => {
        arrived = handler;
        return unlisten;
      },
    };
    return { links, take, unlisten, arrive: () => arrived?.() };
  }

  it("waits until the app is ready, then follows each link held for it", async () => {
    const { links, take, arrive, unlisten } = source([
      ["gents://session/a", "gents://session/b"],
      ["gents://session/c"],
    ]);
    const open = vi.fn(() => true);
    const hook = renderHook(({ ready }) => useOutsideLinks(ready, links, open), {
      initialProps: { ready: false },
    });
    await act(async () => {});
    expect(take).not.toHaveBeenCalled();

    hook.rerender({ ready: true });
    await act(async () => {});
    expect(open.mock.calls).toEqual([["gents://session/a"], ["gents://session/b"]]);

    await act(async () => arrive());
    expect(open).toHaveBeenLastCalledWith("gents://session/c");
    expect(take).toHaveBeenCalledTimes(2);

    hook.unmount();
    await act(async () => {});
    expect(unlisten).toHaveBeenCalled();
  });
});
