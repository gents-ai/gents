/* The plugin route: `/plugins/<id>` draws the page the plugin with that id
   registered in `plugin.pages`, behind a boundary, mounted once; without
   one it says so. The area list the Rust manifest validator accepts and
   the one the desktop knows are the same list, by test, since nothing
   else binds them. */
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { useEffect, useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { createUxContext } from "@/contrib/plugin";
import { FrameViewport } from "@/contrib/react/frame-viewport";
import type { SandboxHost } from "@/contrib/sandbox-host";
import { KNOWN_AREAS, PLUGIN_PAGE_AREA } from "@/contrib/types";
import { PluginPageScreen } from "@/screens/PluginPageScreen";
import { node, publish, renderIn, testApp } from "./app-fixture";

const disposers: Array<() => void> = [];
afterEach(() => {
  disposers.splice(0).forEach((dispose) => dispose());
  vi.restoreAllMocks();
});

describe("the plugin page route", () => {
  it("draws the page the plugin with that id registered, and says so when there is none", () => {
    const board = createUxContext("board", { onDispose: (d) => disposers.push(d) });
    const other = createUxContext("other", { onDispose: (d) => disposers.push(d) });
    board.register({
      id: "page",
      area: PLUGIN_PAGE_AREA,
      render: () => <p data-testid="board-page">the board</p>,
    });
    other.register({
      id: "page",
      area: PLUGIN_PAGE_AREA,
      order: -1,
      render: () => <p data-testid="other-page">another plugin's page</p>,
    });
    const app = testApp();
    const view = renderIn(app, <PluginPageScreen pluginId="board" />);
    expect(screen.getByTestId("board-page")).toHaveTextContent("the board");
    expect(screen.queryByTestId("other-page")).toBeNull();

    view.rerender(<PluginPageScreen pluginId="nobody" />);
    expect(screen.getByTestId("plugin-page-missing")).toHaveTextContent("nobody");
  });

  it("fills the main pane instead of the reading column, and a sandboxed page's frame fills it too", () => {
    const board = createUxContext("board", { onDispose: (d) => disposers.push(d) });
    board.register({
      id: "page",
      area: PLUGIN_PAGE_AREA,
      render: () => <p>the board</p>,
    });
    renderIn(testApp(), <PluginPageScreen pluginId="board" />);
    const pane = screen.getByTestId("plugin-page");
    expect(pane.className).toMatch(/\bh-full\b/);
    expect(pane.className).toMatch(/\bflex-col\b/);
    expect(pane.className).not.toMatch(/max-w-page|mx-auto/);

    /* the frame's anchor: a page grows into the pane, a section keeps its floor */
    const host = {
      show: vi.fn(),
      attach: vi.fn(),
      hide: vi.fn(),
    } as unknown as SandboxHost;
    const view = render(
      <>
        <FrameViewport host={host} surface="board:page" props={{}} fill />
        <FrameViewport host={host} surface="board:section" props={{}} />
      </>,
    );
    const [page, section] = view.getAllByTestId("ux-frame-viewport");
    expect(page!.className).toMatch(/\bflex-1\b/);
    expect(page!.className).not.toMatch(/70vh/);
    expect(page!).toHaveAttribute("data-fill");
    expect(section!.className).toMatch(/min-h-\[70vh\]/);
    expect(section!).not.toHaveAttribute("data-fill");
    view.unmount();
  });

  it("keeps the page mounted while the shell re-renders, and contains a throw", () => {
    let mounts = 0;
    function Page() {
      const [draft, setDraft] = useState("");
      useEffect(() => {
        mounts += 1;
      }, []);
      return <input value={draft} onChange={(e) => setDraft(e.target.value)} />;
    }
    const board = createUxContext("board", { onDispose: (d) => disposers.push(d) });
    board.register({ id: "page", area: PLUGIN_PAGE_AREA, render: () => <Page /> });
    const app = testApp();
    renderIn(app, <PluginPageScreen pluginId="board" />);
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "half a task" } });
    act(() => {
      publish(app, [node({ nodeDid: "did:key:a" })]);
    });
    expect(screen.getByRole("textbox")).toHaveValue("half a task");
    expect(mounts).toBe(1);

    vi.spyOn(console, "error").mockImplementation(() => {});
    const boom = createUxContext("boom", { onDispose: (d) => disposers.push(d) });
    boom.register({
      id: "page",
      area: PLUGIN_PAGE_AREA,
      render: () => {
        throw new Error("page failed");
      },
    });
    renderIn(app, <PluginPageScreen pluginId="boom" />);
    expect(screen.getByTestId("contrib-error-pane")).toHaveTextContent("page failed");
  });
});

describe("the area vocabulary", () => {
  it("is one list on both sides of the bridge", () => {
    const rust = readFileSync(
      join(__dirname, "../../../crates/gents/src/pack.rs"),
      "utf8",
    );
    const block = /pub const UX_PLUGIN_AREAS: &\[&str\] = &\[([^\]]*)\];/.exec(rust);
    expect(block, "UX_PLUGIN_AREAS in pack.rs").not.toBeNull();
    const areas = [...block![1]!.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
    expect(areas.sort()).toEqual([...KNOWN_AREAS].sort());
  });
});
