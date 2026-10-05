import { fireEvent, render, screen, within } from "@testing-library/react";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { DockTabs } from "@/app/Dock";
import { ShellProvider } from "@/app/ShellContext";
import { clearSurfaces, registerSurface } from "@/app/surfaces";
import { workspace } from "@/app/workspace";
import type { Shell } from "@/hooks/useShell";

const Nothing = () => null;
const SCOPE = "session:a";
const shell = { deployments: [] } as unknown as Shell;

const renderTabs = () =>
  render(
    <ShellProvider value={shell}>
      <DockTabs sessionId="a" routeName="session" />
    </ShellProvider>,
  );
const tab = (name: string) => screen.getByRole("tab", { name });
const capture = Element.prototype.setPointerCapture;

describe("the dock's tab strip", () => {
  beforeAll(() => {
    Element.prototype.setPointerCapture = () => {};
  });
  afterAll(() => {
    Element.prototype.setPointerCapture = capture;
  });
  beforeEach(() => {
    clearSurfaces();
    workspace.reset();
    for (const [id, title] of [
      ["trace", "Trace"],
      ["workers", "Workers"],
      ["diagnostics", "Diagnostics"],
    ]) {
      registerSurface({
        id,
        title,
        icon: Nothing,
        placements: ["dock"],
        render: Nothing,
      });
      workspace.openSurface(SCOPE, id);
    }
    workspace.activate(SCOPE, "trace");
  });

  it("holds only tabs, one of them in the tab order", () => {
    renderTabs();
    const tablist = screen.getByRole("tablist", { name: "Panels" });
    expect(
      within(tablist)
        .getAllByRole("tab")
        .map((t) => t.textContent),
    ).toEqual(["Trace", "Workers", "Diagnostics"]);
    expect(within(tablist).queryAllByRole("button")).toEqual([]);
    expect(tab("Trace")).toHaveAttribute("tabindex", "0");
    expect(tab("Workers")).toHaveAttribute("tabindex", "-1");
  });

  it("moves between tabs with the arrows, Home and End, showing the one reached", () => {
    renderTabs();
    tab("Trace").focus();
    fireEvent.keyDown(tab("Trace"), { key: "ArrowRight" });
    expect(workspace.dock(SCOPE).active).toBe("workers");
    expect(tab("Workers")).toHaveFocus();
    expect(tab("Workers")).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(tab("Workers"), { key: "End" });
    expect(tab("Diagnostics")).toHaveFocus();
    fireEvent.keyDown(tab("Diagnostics"), { key: "ArrowRight" });
    expect(tab("Trace")).toHaveFocus();
    fireEvent.keyDown(tab("Trace"), { key: "ArrowLeft" });
    expect(tab("Diagnostics")).toHaveFocus();
    fireEvent.keyDown(tab("Diagnostics"), { key: "Home" });
    expect(tab("Trace")).toHaveFocus();
    expect(workspace.dock(SCOPE).active).toBe("trace");
  });

  it("closes the focused tab with Delete and focuses the one showing next", () => {
    renderTabs();
    tab("Trace").focus();
    fireEvent.keyDown(tab("Trace"), { key: "Delete" });
    expect(workspace.dock(SCOPE).tabs).toEqual(["workers", "diagnostics"]);
    expect(tab("Workers")).toHaveFocus();
  });

  it("focuses a tab pressed with the pointer", () => {
    renderTabs();
    fireEvent.pointerDown(tab("Workers"), { button: 0, pointerId: 1 });
    expect(tab("Workers")).toHaveFocus();
    expect(workspace.dock(SCOPE).active).toBe("workers");
  });
});
