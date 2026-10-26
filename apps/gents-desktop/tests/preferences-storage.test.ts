import { waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { restoreListViews } from "../src/ui/app/listViews";
import { emptyFilter } from "../src/ui/lib/session-filter";
import {
  applyChosenTheme,
  followOtherWindows,
  resetPreferences,
  restorePreferences,
  zoomLevel,
  type Preferences,
} from "../src/ui/preferences";
import { onZoomKeys, stepZoom, zoomKey } from "../src/ui/lib/zoom";

const defaults: Preferences = {
  theme: "light",
  nav: "hover",
  zoom: 1,
  sentTurn: "follow",
  followUp: "steer",
};

describe("preferences read back from storage", () => {
  it("keeps the defaults for values that are not preferences", () => {
    expect(restorePreferences({ theme: "sepia", nav: 3 }, defaults)).toEqual(defaults);
    expect(restorePreferences(null, defaults)).toEqual(defaults);
    expect(restorePreferences({ theme: "dark", nav: "collapsed" }, defaults)).toEqual({
      theme: "dark",
      nav: "collapsed",
      zoom: 1,
      sentTurn: "follow",
      followUp: "steer",
    });
    expect(restorePreferences({ sentTurn: "anchor" }, defaults).sentTurn).toBe(
      "anchor",
    );
    expect(restorePreferences({ sentTurn: "top" }, defaults).sentTurn).toBe("follow");
    expect(restorePreferences({ followUp: "queue" }, defaults).followUp).toBe("queue");
    expect(restorePreferences({ followUp: "later" }, defaults).followUp).toBe("steer");
  });

  it("keeps a zoom only when it is one of the steps", () => {
    expect(restorePreferences({ zoom: 1.25 }, defaults).zoom).toBe(1.25);
    expect(restorePreferences({ zoom: 7 }, defaults).zoom).toBe(1);
    expect(restorePreferences({ zoom: "1.25" }, defaults).zoom).toBe(1);
  });
});

describe("zooming", () => {
  it("steps up and down through the browser's steps, and resets to actual size", () => {
    expect(stepZoom(1, "in")).toBe(1.1);
    expect(stepZoom(1, "out")).toBe(0.9);
    expect(stepZoom(2, "in")).toBe(2);
    expect(stepZoom(0.67, "out")).toBe(0.67);
    expect(stepZoom(1.5, "reset")).toBe(1);
  });

  it("takes Ctrl with =, +, − and 0 where no menu carries the keys", () => {
    const key = (key: string, mods: Partial<KeyboardEvent> = {}) =>
      zoomKey({ key, ctrlKey: true, metaKey: false, altKey: false, ...mods });
    expect([key("="), key("+"), key("-"), key("0")]).toEqual([
      "in",
      "in",
      "out",
      "reset",
    ]);
    expect(key("=", { ctrlKey: false })).toBeNull();
    expect(key("=", { altKey: true })).toBeNull();
    expect(key("1")).toBeNull();

    const steps: string[] = [];
    const stop = onZoomKeys(window, (action) => steps.push(action));
    const press = new KeyboardEvent("keydown", {
      key: "=",
      ctrlKey: true,
      cancelable: true,
    });
    document.body.dispatchEvent(press);
    document.body.dispatchEvent(
      new KeyboardEvent("keydown", { key: "a", ctrlKey: true }),
    );
    stop();
    document.body.dispatchEvent(
      new KeyboardEvent("keydown", { key: "0", ctrlKey: true }),
    );
    expect(steps).toEqual(["in"]);
    expect(press.defaultPrevented).toBe(true);
  });
});

describe("another window's preferences", () => {
  it("are read back when that window saves them, and its theme is shown", async () => {
    const values = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
      removeItem: (key: string) => values.delete(key),
    });
    const stop = followOtherWindows();
    localStorage.setItem(
      "gents-preferences",
      JSON.stringify({
        state: { theme: "dark", nav: "hover", zoom: 1.25 },
        version: 1,
      }),
    );
    window.dispatchEvent(new StorageEvent("storage", { key: "gents-preferences" }));
    await waitFor(() => expect(document.documentElement.dataset.theme).toBe("dark"));
    expect(zoomLevel()).toBe(1.25);
    stop();
    resetPreferences();
    applyChosenTheme();
    vi.unstubAllGlobals();
  });
});

describe("list views read back from storage", () => {
  it("drops a session state or source this build does not offer", () => {
    const restored = restoreListViews({
      sessionFilter: {
        states: ["held", "live"],
        sources: ["robot", "task"],
        agents: [],
      },
    });
    expect(restored.sessionFilter).toEqual({
      states: ["working", "waiting"],
      sources: ["task"],
      agents: [],
    });
  });

  it("leaves out what is not a list of strings, and keeps an empty node pick", () => {
    expect(restoreListViews({ sessionNodes: "a", mailboxNodes: ["a", 1] })).toEqual({
      sessionFilter: emptyFilter,
      sessionNodes: null,
      mailboxNodes: ["a"],
      mailboxKinds: [],
    });
    expect(restoreListViews({ sessionNodes: [] }).sessionNodes).toEqual([]);
  });
});
