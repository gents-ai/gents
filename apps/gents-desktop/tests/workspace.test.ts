import { beforeEach, describe, expect, it } from "vitest";
import { workspace } from "@/app/workspace";
import {
  clearSurfaces,
  getSurface,
  listSurfaces,
  registerSurface,
} from "@/app/surfaces";

const Nothing = () => null;

describe("the workspace store", () => {
  beforeEach(() => workspace.reset());

  it("starts with the dock closed and empty", () => {
    expect(workspace.get().dock).toEqual({ open: false, tabs: [], active: null });
  });

  it("opens a surface as a tab and shows it; opening it again only shows it", () => {
    workspace.openSurface("trace");
    workspace.openSurface("workers");
    workspace.openSurface("trace");
    expect(workspace.get().dock).toEqual({
      open: true,
      tabs: ["trace", "workers"],
      active: "trace",
    });
  });

  it("closing the showing tab shows its neighbour, and the last one closes the dock", () => {
    workspace.openSurface("trace");
    workspace.openSurface("workers");
    workspace.openSurface("diagnostics");
    workspace.closeTab("diagnostics");
    expect(workspace.get().dock).toMatchObject({
      open: true,
      tabs: ["trace", "workers"],
      active: "workers",
    });
    workspace.closeTab("trace");
    expect(workspace.get().dock).toMatchObject({
      tabs: ["workers"],
      active: "workers",
    });
    workspace.closeTab("workers");
    expect(workspace.get().dock).toEqual({ open: false, tabs: [], active: null });
  });

  it("moves a tab to another position, clamped, and keeps the one showing", () => {
    workspace.openSurface("trace");
    workspace.openSurface("workers");
    workspace.openSurface("diagnostics");
    workspace.activate("trace");
    workspace.moveTab("trace", 2);
    expect(workspace.get().dock).toMatchObject({
      tabs: ["workers", "diagnostics", "trace"],
      active: "trace",
    });
    workspace.moveTab("diagnostics", -5);
    expect(workspace.get().dock.tabs).toEqual(["diagnostics", "workers", "trace"]);
    workspace.moveTab("missing", 0);
    expect(workspace.get().dock.tabs).toEqual(["diagnostics", "workers", "trace"]);
  });

  it("closing the dock keeps its tabs for reopening", () => {
    workspace.openSurface("trace");
    workspace.closeDock();
    expect(workspace.get().dock).toMatchObject({ open: false, tabs: ["trace"] });
    workspace.reopenDock();
    expect(workspace.get().dock.open).toBe(true);
  });

  it("tells subscribers once per change", () => {
    let n = 0;
    const off = workspace.subscribe(() => n++);
    workspace.openSurface("trace");
    workspace.closeDock();
    off();
    workspace.openSurface("trace");
    expect(n).toBe(2);
  });
});

describe("the surface registry", () => {
  beforeEach(() => clearSurfaces());

  it("finds a registered surface by id and lists by placement", () => {
    registerSurface({
      id: "trace",
      title: "Trace",
      icon: Nothing,
      placements: ["dock"],
      render: Nothing,
    });
    registerSurface({
      id: "diff",
      title: "Diff",
      icon: Nothing,
      placements: ["dock", "inline"],
      render: Nothing,
    });
    expect(getSurface("trace")?.title).toBe("Trace");
    expect(getSurface("missing")).toBeNull();
    expect(listSurfaces("inline").map((s) => s.id)).toEqual(["diff"]);
    expect(listSurfaces().length).toBe(2);
  });
});
