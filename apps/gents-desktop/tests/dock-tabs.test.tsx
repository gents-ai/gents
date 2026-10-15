import { fireEvent, render, screen, within } from "@testing-library/react";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { DockTabs } from "@/app/Dock";
import { clearSurfaces, registerSurface } from "@/app/surfaces";
import { workspace } from "@/app/workspace";
import { testApp, withApp } from "./app-fixture";

const Nothing = () => null;
const SCOPE = "session:a";
const renderTabs = () =>
  render(<DockTabs sessionId="a" routeName="session" />, {
    wrapper: withApp(testApp()),
  });
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
      ["workers", "Started sessions"],
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
    ).toEqual(["Trace", "Started sessions", "Diagnostics"]);
    expect(within(tablist).queryAllByRole("button")).toEqual([]);
    expect(tab("Trace")).toHaveAttribute("tabindex", "0");
    expect(tab("Started sessions")).toHaveAttribute("tabindex", "-1");
  });

  it("moves between tabs with the arrows, Home and End, showing the one reached", () => {
    renderTabs();
    tab("Trace").focus();
    fireEvent.keyDown(tab("Trace"), { key: "ArrowRight" });
    expect(workspace.dock(SCOPE).active).toBe("workers");
    expect(tab("Started sessions")).toHaveFocus();
    expect(tab("Started sessions")).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(tab("Started sessions"), { key: "End" });
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
    expect(tab("Started sessions")).toHaveFocus();
  });

  it("focuses a tab pressed with the pointer", () => {
    renderTabs();
    fireEvent.pointerDown(tab("Started sessions"), { button: 0, pointerId: 1 });
    expect(tab("Started sessions")).toHaveFocus();
    expect(workspace.dock(SCOPE).active).toBe("workers");
  });
});
