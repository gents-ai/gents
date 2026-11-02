import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { useNavResize } from "../src/ui/app/useNavResize";
import { Shortcuts } from "../src/ui/screens/Shortcuts";
import { preferences, resetPreferences, usePreferences } from "../src/ui/preferences";
import { renderIn, testApp } from "./app-fixture";

afterEach(() => act(() => resetPreferences()));

const state = () => {
  let read = { navHidden: false, navWidth: 0, nav: "" };
  function Probe() {
    read = {
      navHidden: usePreferences((p) => p.navHidden),
      navWidth: usePreferences((p) => p.navWidth),
      nav: usePreferences((p) => p.nav),
    };
    return null;
  }
  render(<Probe />);
  return () => read;
};

function Handle({ most = Infinity }: { most?: number }) {
  /* the rail's edge while the sidebar isn't expanded, as the shell has it */
  const nav = usePreferences((p) => p.nav);
  const resize = useNavResize(
    usePreferences((p) => p.navWidth),
    most,
    nav === "collapsed",
  );
  return <div {...resize.handleProps} aria-label="Resize sidebar" />;
}

describe("the sidebar", () => {
  it("is hidden and shown again by its key in any nav mode", () => {
    const read = state();
    renderIn(testApp(), <Shortcuts />);
    for (const nav of ["hover", "expanded", "collapsed"] as const) {
      act(() => preferences.setNav(nav));
      fireEvent.keyDown(window, { key: "S", ctrlKey: true, shiftKey: true });
      expect(read().navHidden).toBe(true);
      fireEvent.keyDown(window, { key: "S", ctrlKey: true, shiftKey: true });
      expect(read().navHidden).toBe(false);
    }
    expect(read().nav).toBe("collapsed");
  });

  it("is named in the shortcuts reference", () => {
    renderIn(testApp(), <Shortcuts />);
    fireEvent.keyDown(window, { key: "/", ctrlKey: true });
    expect(screen.getByText("Show or hide the sidebar")).toBeVisible();
    expect(screen.getByText("Ctrl+Shift+S")).toBeVisible();
  });

  it("widens and narrows from its edge's keys, within its range", () => {
    const read = state();
    render(<Handle />);
    const handle = screen.getByRole("separator", { name: "Resize sidebar" });
    expect(handle).toHaveAttribute("aria-valuenow", "304");
    fireEvent.keyDown(handle, { key: "ArrowRight" });
    expect(read().navWidth).toBe(320);
    fireEvent.keyDown(handle, { key: "ArrowLeft", shiftKey: true });
    expect(read().navWidth).toBe(256);
    fireEvent.keyDown(handle, { key: "End" });
    expect(read().navWidth).toBe(420);
    fireEvent.keyDown(handle, { key: "Home" });
    expect(read().navWidth).toBe(220);
  });

  it("goes to icons only past its minimum, and comes out again from the rail", () => {
    const read = state();
    act(() => preferences.setNav("expanded"));
    render(<Handle />);
    const handle = screen.getByRole("separator", { name: "Resize sidebar" });
    fireEvent.keyDown(handle, { key: "Home" });
    fireEvent.keyDown(handle, { key: "ArrowLeft" });
    expect(read().nav).toBe("collapsed");
    expect(read().navWidth).toBe(220);
    fireEvent.keyDown(handle, { key: "ArrowRight" });
    expect(read().nav).toBe("expanded");
  });

  it("goes back to its first width on a double-click", () => {
    const read = state();
    act(() => preferences.setNavWidth(400));
    render(<Handle />);
    fireEvent.doubleClick(screen.getByRole("separator", { name: "Resize sidebar" }));
    expect(read().navWidth).toBe(304);
  });

  it("leaves the page its room: a narrow window caps the width it shows", () => {
    act(() => preferences.setNavWidth(400));
    render(<Handle most={260} />);
    const handle = screen.getByRole("separator", { name: "Resize sidebar" });
    expect(handle).toHaveAttribute("aria-valuenow", "260");
    expect(handle).toHaveAttribute("aria-valuemax", "260");
  });
});
