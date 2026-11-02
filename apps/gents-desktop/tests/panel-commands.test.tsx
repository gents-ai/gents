import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Route } from "@/lib/router";
import type { ShellLayout } from "@/app/useShellLayout";

const Nothing = () => null;
const SCOPE = "session:a";
const SESSION: Route = { name: "session", sessionId: "a" };

const desktop = (on: boolean) => {
  if (on) (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
  else delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
};

/* after the reset, so the commands, the panel and the app share one module
   graph */
async function load(route: Route = SESSION) {
  vi.resetModules();
  const router = await import("../src/ui/lib/router");
  vi.spyOn(router, "currentRoute").mockReturnValue(route);
  const surfaces = await import("../src/ui/app/surfaces");
  const { workspace } = await import("../src/ui/app/workspace");
  const { PanelPicker, usePanelCommands } = await import("../src/ui/app/panelCommands");
  const { DockTabs } = await import("../src/ui/app/Dock");
  const { CommandKeys } = await import("../src/ui/commands/dispatch");
  const commands = await import("../src/ui/commands/commands");
  const fixture = await import("./app-fixture");
  surfaces.clearSurfaces();
  workspace.reset();
  for (const [id, title] of [
    ["workers", "Started sessions"],
    ["background", "Background"],
    ["changes", "Changes"],
  ] as const)
    surfaces.registerSurface({
      id,
      title,
      icon: Nothing,
      placements: ["dock"],
      routes: ["session"],
      render: Nothing,
    });
  const divider = { toEnd: 0, showPane: vi.fn(), hidePane: vi.fn() };
  const layout = { docked: true, divider } as unknown as ShellLayout;
  function Shell() {
    usePanelCommands(route, layout);
    return (
      <>
        <input aria-label="Composer" />
        {route.name === "session" && <DockTabs sessionId="a" routeName="session" />}
        <PanelPicker route={route} />
        <CommandKeys />
      </>
    );
  }
  fixture.renderIn(fixture.testApp(), <Shell />);
  return { workspace, divider, surfaces, commands };
}

/* jsdom is not a Mac: the command key is Ctrl, and Alt goes with a number */
const press = (key: string, mods: KeyboardEventInit = {}) =>
  act(() => {
    fireEvent.keyDown(document.activeElement ?? window, { key, ...mods });
  });
const cmd = (key: string, shift = false) =>
  press(key, { ctrlKey: true, shiftKey: shift });

describe("the panel's commands", () => {
  beforeEach(() => desktop(true));
  afterEach(() => desktop(false));

  it("shows and hides the panel, giving focus back, on the tab used last", async () => {
    const { workspace } = await load();
    act(() => workspace.openSurface(SCOPE, "changes"));
    act(() => workspace.closeDock(SCOPE));
    const composer = screen.getByRole("textbox", { name: "Composer" });
    composer.focus();
    cmd("j");
    expect(workspace.dock(SCOPE)).toMatchObject({ open: true, active: "changes" });
    await waitFor(() =>
      expect(screen.getByRole("tab", { name: /Changes/ })).toHaveFocus(),
    );
    cmd("j");
    expect(workspace.dock(SCOPE).open).toBe(false);
    expect(composer).toHaveFocus();
  }, 20_000);

  it("opens the picker, which finds a panel by name and opens it", async () => {
    const { workspace } = await load();
    cmd("J", true);
    const find = await screen.findByRole("combobox", { name: "Find a panel" });
    expect(screen.getAllByRole("option").map((o) => o.textContent)).toEqual([
      "Started sessions1",
      "Background2",
      "Changes3",
    ]);
    fireEvent.change(find, { target: { value: "back" } });
    expect(screen.getAllByRole("option")).toHaveLength(1);
    fireEvent.keyDown(find, { key: "Enter" });
    expect(workspace.dock(SCOPE)).toMatchObject({ open: true, active: "background" });
    await waitFor(() => expect(screen.queryByTestId("panel-picker")).toBeNull());
  }, 20_000);

  it("opens a panel by its number in the picker, and closes with Esc", async () => {
    const { workspace } = await load();
    act(() => workspace.openSurface(SCOPE, "changes"));
    cmd("J", true);
    const find = await screen.findByRole("combobox", { name: "Find a panel" });
    /* the open tab comes first, marked open */
    expect(screen.getAllByRole("option")[0]).toHaveTextContent("ChangesOpen1");
    fireEvent.keyDown(find, { key: "3" });
    expect(workspace.dock(SCOPE).active).toBe("background");
    cmd("J", true);
    fireEvent.keyDown(await screen.findByRole("combobox", { name: "Find a panel" }), {
      key: "Escape",
    });
    await waitFor(() => expect(screen.queryByTestId("panel-picker")).toBeNull());
  }, 20_000);

  it("goes straight to a panel by its number", async () => {
    const { workspace } = await load();
    press("2", { altKey: true });
    expect(workspace.dock(SCOPE)).toMatchObject({ open: true, active: "background" });
    /* a number past the panels this page has stays with the page */
    press("9", { altKey: true });
    expect(workspace.dock(SCOPE).active).toBe("background");
  }, 20_000);

  it("gives every panel registered, a plugin's too, an Open command", async () => {
    const { surfaces, commands } = await load();
    act(() => {
      surfaces.registerSurface({
        id: "plugin.notes",
        title: "Notes",
        icon: Nothing,
        placements: ["dock"],
        render: Nothing,
      });
    });
    await waitFor(() =>
      expect(commands.getCommand("panel.open.plugin.notes")?.title).toBe(
        "Open Notes panel",
      ),
    );
  }, 20_000);

  it("expands the shown panel, and restores it", async () => {
    const { workspace, divider, commands } = await load();
    const expand = () => commands.getCommand("panel.expand")!;
    expect(expand().when!()).toBe(false);
    act(() => workspace.openSurface(SCOPE, "background"));
    await waitFor(() => expect(expand().when!()).toBe(true));
    act(() => void expand().run(0));
    expect(divider.hidePane).toHaveBeenCalledTimes(1);
  }, 20_000);

  it("leaves its keys to a browser", async () => {
    desktop(false);
    const { workspace } = await load();
    cmd("j");
    cmd("J", true);
    press("1", { altKey: true });
    expect(workspace.dock(SCOPE).open).toBe(false);
    expect(screen.queryByTestId("panel-picker")).toBeNull();
  }, 20_000);
});
