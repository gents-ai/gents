import { act, fireEvent, render, screen, within } from "@testing-library/react";
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

describe("dragging a dock tab", () => {
  const rect = Element.prototype.getBoundingClientRect;
  beforeAll(() => {
    Element.prototype.setPointerCapture = () => {};
    /* jsdom lays nothing out: each tab takes a 100px slot by its place */
    Element.prototype.getBoundingClientRect = function (this: Element) {
      const strip = this.parentElement
        ? [...this.parentElement.querySelectorAll("[data-reorder-key]")]
        : [];
      const at = Math.max(0, strip.indexOf(this));
      return {
        left: at * 100,
        top: 0,
        width: 100,
        height: 30,
        right: at * 100 + 100,
        bottom: 30,
        x: at * 100,
        y: 0,
        toJSON: () => ({}),
      } as DOMRect;
    };
  });
  afterAll(() => {
    Element.prototype.setPointerCapture = capture;
    Element.prototype.getBoundingClientRect = rect;
  });
  beforeEach(() => {
    clearSurfaces();
    workspace.reset();
    for (const [id, title] of [
      ["trace", "Trace"],
      ["workers", "Started sessions"],
      ["diagnostics", "Diagnostics"],
      ["notes", "Notes"],
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
    workspace.activate(SCOPE, "workers");
  });

  it("keeps the held tab when another closes, and drops it where it is let go", () => {
    renderTabs();
    const held = tab("Started sessions");
    fireEvent.pointerDown(held, { button: 0, pointerId: 1, clientX: 150 });
    fireEvent.pointerMove(held, { pointerId: 1, clientX: 250 });
    /* the first tab closes from the keyboard while the pointer is still
       down; the strip closes up under the pointer */
    act(() => workspace.closeTab(SCOPE, "trace"));
    /* over the second slot of what is left, which Diagnostics now holds */
    fireEvent.pointerMove(held, { pointerId: 1, clientX: 150 });
    fireEvent.pointerUp(held, { pointerId: 1, clientX: 150 });
    expect(workspace.dock(SCOPE).tabs).toEqual(["diagnostics", "workers", "notes"]);
  });

  it("moves nothing when the held tab itself closes", () => {
    renderTabs();
    const held = tab("Started sessions");
    fireEvent.pointerDown(held, { button: 0, pointerId: 1, clientX: 150 });
    fireEvent.pointerMove(held, { pointerId: 1, clientX: 250 });
    act(() => workspace.closeTab(SCOPE, "workers"));
    fireEvent.pointerUp(held, { pointerId: 1, clientX: 250 });
    expect(workspace.dock(SCOPE).tabs).toEqual(["trace", "diagnostics", "notes"]);
  });

  it("goes on with the drag when a tab opens while one is held", () => {
    registerSurface({
      id: "extra",
      title: "Extra",
      icon: Nothing,
      placements: ["dock"],
      render: Nothing,
    });
    renderTabs();
    const held = tab("Started sessions");
    fireEvent.pointerDown(held, { button: 0, pointerId: 1, clientX: 150 });
    fireEvent.pointerMove(held, { pointerId: 1, clientX: 250 });
    act(() => workspace.openSurface(SCOPE, "extra"));
    fireEvent.pointerUp(held, { pointerId: 1, clientX: 250 });
    expect(workspace.dock(SCOPE).tabs).toEqual([
      "trace",
      "diagnostics",
      "workers",
      "notes",
      "extra",
    ]);
  });

  it("still moves the held tab when nothing else changed", () => {
    renderTabs();
    const held = tab("Started sessions");
    fireEvent.pointerDown(held, { button: 0, pointerId: 1, clientX: 150 });
    fireEvent.pointerMove(held, { pointerId: 1, clientX: 250 });
    fireEvent.pointerUp(held, { pointerId: 1, clientX: 250 });
    expect(workspace.dock(SCOPE).tabs).toEqual([
      "trace",
      "diagnostics",
      "workers",
      "notes",
    ]);
  });
});
