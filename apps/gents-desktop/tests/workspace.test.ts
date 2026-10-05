import { beforeEach, describe, expect, it, vi } from "vitest";
import { dockScope, workspace } from "@/app/workspace";
import {
  clearSurfaces,
  getSurface,
  listSurfaces,
  registerSurface,
} from "@/app/surfaces";

const Nothing = () => null;

const A = "session:a";

describe("the workspace store", () => {
  beforeEach(() => workspace.reset());

  it("starts with every dock closed and empty", () => {
    expect(workspace.dock(A)).toEqual({ open: false, tabs: [], active: null });
  });

  it("opens a surface as a tab and shows it; opening it again only shows it", () => {
    workspace.openSurface(A, "trace");
    workspace.openSurface(A, "workers");
    workspace.openSurface(A, "trace");
    expect(workspace.dock(A)).toEqual({
      open: true,
      tabs: ["trace", "workers"],
      active: "trace",
    });
  });

  it("closing the showing tab shows its neighbour, and the last one closes the dock", () => {
    workspace.openSurface(A, "trace");
    workspace.openSurface(A, "workers");
    workspace.openSurface(A, "diagnostics");
    workspace.closeTab(A, "diagnostics");
    expect(workspace.dock(A)).toMatchObject({
      open: true,
      tabs: ["trace", "workers"],
      active: "workers",
    });
    workspace.closeTab(A, "trace");
    expect(workspace.dock(A)).toMatchObject({ tabs: ["workers"], active: "workers" });
    workspace.closeTab(A, "workers");
    expect(workspace.dock(A)).toEqual({ open: false, tabs: [], active: null });
  });

  it("moves a tab to another position, clamped, and keeps the one showing", () => {
    workspace.openSurface(A, "trace");
    workspace.openSurface(A, "workers");
    workspace.openSurface(A, "diagnostics");
    workspace.activate(A, "trace");
    workspace.moveTab(A, "trace", 2);
    expect(workspace.dock(A)).toMatchObject({
      tabs: ["workers", "diagnostics", "trace"],
      active: "trace",
    });
    workspace.moveTab(A, "diagnostics", -5);
    expect(workspace.dock(A).tabs).toEqual(["diagnostics", "workers", "trace"]);
    workspace.moveTab(A, "missing", 0);
    expect(workspace.dock(A).tabs).toEqual(["diagnostics", "workers", "trace"]);
  });

  it("closing the dock keeps its tabs for reopening", () => {
    workspace.openSurface(A, "trace");
    workspace.closeDock(A);
    expect(workspace.dock(A)).toMatchObject({ open: false, tabs: ["trace"] });
    workspace.reopenDock(A);
    expect(workspace.dock(A).open).toBe(true);
  });
});

describe("each session's dock", () => {
  beforeEach(() => workspace.reset());

  it("finds a screen's dock from its route: a session's own, or the shared one", () => {
    expect(dockScope({ name: "session", sessionId: "a" })).toBe("session:a");
    expect(dockScope({ name: "session", sessionId: null })).toBe("session:new");
    expect(dockScope({ name: "sessions" })).toBe(dockScope({ name: "agents" }));
  });

  it("changes only the dock it names", () => {
    const shared = dockScope({ name: "sessions" });
    workspace.openSurface(A, "trace");
    workspace.openSurface("session:b", "workers");
    workspace.closeDock("session:b");
    expect(workspace.dock(A)).toEqual({ open: true, tabs: ["trace"], active: "trace" });
    expect(workspace.dock("session:b")).toMatchObject({
      open: false,
      tabs: ["workers"],
    });
    expect(workspace.dock(shared)).toEqual({ open: false, tabs: [], active: null });
  });

  it("gives a session created from the new-session screen the dock it was composed with", () => {
    workspace.openSurface("session:new", "trace");
    workspace.adoptNewSessionDock("created");
    expect(workspace.dock("session:created").tabs).toEqual(["trace"]);
    expect(workspace.dock("session:new").tabs).toEqual([]);
  });

  it("remembers the tabs across runs but not that the dock was open", () => {
    const saved = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => saved.get(key) ?? null,
      setItem: (key: string, value: string) => saved.set(key, value),
      removeItem: (key: string) => saved.delete(key),
    });
    workspace.openSurface(A, "trace");
    vi.unstubAllGlobals();
    const persisted = JSON.parse(saved.get("gents-dock-by-scope") ?? "{}");
    expect(persisted.state.docks[A]).toEqual({
      open: false,
      tabs: ["trace"],
      active: "trace",
    });
  });

  it("forgets the docks of sessions not visited in a long while", () => {
    workspace.visit("session:first");
    workspace.openSurface("session:first", "trace");
    for (let i = 0; i < 200; i += 1) workspace.visit(`session:${i}`);
    expect(workspace.dock("session:first").tabs).toEqual([]);
    workspace.openSurface("session:199", "trace");
    workspace.visit("session:another");
    expect(workspace.dock("session:199").tabs).toEqual(["trace"]);
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
