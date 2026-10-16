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
import { stepZoom } from "../src/ui/lib/zoom";

const defaults: Preferences = { theme: "light", nav: "hover", zoom: 1 };

describe("preferences read back from storage", () => {
  it("keeps the defaults for values that are not preferences", () => {
    expect(restorePreferences({ theme: "sepia", nav: 3 }, defaults)).toEqual(defaults);
    expect(restorePreferences(null, defaults)).toEqual(defaults);
    expect(restorePreferences({ theme: "dark", nav: "collapsed" }, defaults)).toEqual({
      theme: "dark",
      nav: "collapsed",
      zoom: 1,
    });
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
    /* "live" is every unsettled session: working or waiting */
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
      openSessions: [],
    });
    expect(restoreListViews({ sessionNodes: [] }).sessionNodes).toEqual([]);
  });
});
