import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const desktop = (on: boolean) => {
  if (on) (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
  else delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
};

const JUNO = "did:key:z6MkJuno";

/* after the reset, so the commands, the palette and the app share one
   module graph, with the desktop app's keys */
async function load() {
  vi.resetModules();
  const fixture = await import("./app-fixture");
  const { deployment } = await import("./config-panel-wiring/fixtures");
  const { Shortcuts } = await import("../src/ui/screens/Shortcuts");
  const { SearchPalette } = await import("../src/ui/screens/SearchPalette");
  const router = await import("../src/ui/lib/router");
  const commands = await import("../src/ui/commands/commands");
  const dispatch = await import("../src/ui/commands/dispatch");
  const app = fixture.testApp({
    deployments: [
      fixture.node(),
      fixture.node({
        nodeDid: JUNO,
        label: "Juno",
        source: "enrollment",
        node: { ...deployment.node, nodeDid: JUNO, displayName: null },
      }),
    ],
    selection: { nodeDid: deployment.nodeDid },
  });
  fixture.renderIn(
    app,
    <>
      <Shortcuts />
      <SearchPalette />
    </>,
  );
  const node = () => app.stores.selection.getState().nodeDid;
  return { app, node, home: deployment.nodeDid, router, commands, dispatch };
}

/* jsdom is not a Mac: the command key is Ctrl */
const press = (key: string, shift = false) =>
  act(() => {
    fireEvent.keyDown(window, { key, ctrlKey: true, shiftKey: shift });
  });

describe("the node keys", () => {
  let unbind: (() => void) | null = null;
  beforeEach(() => desktop(true));
  afterEach(() => {
    unbind?.();
    unbind = null;
    desktop(false);
  });

  it("open search on the nodes, the one in use first and marked", async () => {
    const { node } = await load();
    press("o");
    expect(await screen.findByRole("radio", { name: "Nodes" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    const rows = screen.getAllByRole("option");
    expect(rows[0]).toHaveTextContent("Current");
    expect(rows[1]).toHaveTextContent("Juno");
    fireEvent.keyDown(screen.getByRole("combobox", { name: "Search" }), {
      key: "ArrowDown",
    });
    fireEvent.keyDown(screen.getByRole("combobox", { name: "Search" }), {
      key: "Enter",
    });
    await waitFor(() => expect(node()).toBe(JUNO));
  }, 20_000);

  it("go back to the node used before, with the key the list names", async () => {
    const { app, node, home } = await load();
    act(() => app.actions.selectNode(JUNO));
    expect(node()).toBe(JUNO);
    press("O", true);
    await waitFor(() => expect(node()).toBe(home));
    /* the list names the key on the node it goes back to */
    press("o");
    const juno = (await screen.findAllByRole("option")).find((o) =>
      o.textContent?.includes("Juno"),
    );
    expect(juno).toHaveTextContent("Ctrl+Shift+O");
  }, 20_000);

  it("ask first when the switch would leave a draft behind", async () => {
    const { app, node, home, router } = await load();
    act(() => app.actions.selectNode(JUNO));
    act(() => app.actions.selectNode(home));
    const asked: unknown[] = [];
    unbind = router.bindNavigationGuard((route) => {
      asked.push(route);
      return false;
    });
    press("O", true);
    expect(asked).toEqual([{ name: "sessions", nodeDid: JUNO }]);
    expect(node()).toBe(home);
  }, 20_000);

  it("do nothing with ⌃O on a Mac, which belongs to text editing", async () => {
    const { commands, dispatch } = await load();
    const e = new KeyboardEvent("keydown", {
      key: "o",
      ctrlKey: true,
      cancelable: true,
    });
    expect(dispatch.dispatchKey(e, commands.listCommands(), true)).toBe(false);
    expect(e.defaultPrevented).toBe(false);
  }, 20_000);
});
