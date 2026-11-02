import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { FLAGS } from "../src/ui/lib/flags";
import {
  flag,
  followOtherWindows,
  preferences,
  resetPreferences,
  restorePreferences,
  useFlag,
  type Preferences,
} from "../src/ui/preferences";
import { withFlags } from "./app-fixture";

const DEMO = "demo-feature";

describe("the experimental features' registry", () => {
  it("names each flag once, in kebab-case, with an owner and a removal date", () => {
    const ids = FLAGS.map((f) => f.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const f of FLAGS) {
      expect(f.id).toMatch(/^[a-z0-9]+(-[a-z0-9]+)*$/);
      expect(f.owner.trim()).not.toBe("");
      expect(f.removeWhen.trim()).not.toBe("");
      expect(f.removeBy, `flag "${f.id}"`).toMatch(/^\d{4}-\d{2}-\d{2}$/);
      expect(Number.isNaN(Date.parse(f.removeBy)), `flag "${f.id}"`).toBe(false);
    }
  });

  it.each(FLAGS.map((f) => [f.id, f] as const))(
    "has not outlived its removal date: %s",
    (_, f) => {
      expect(
        Date.parse(`${f.removeBy}T23:59:59Z`) >= Date.now(),
        `Flag "${f.id}" (owner: ${f.owner}) was due to go by ${f.removeBy}. ` +
          `Remove it when: ${f.removeWhen} Ship what it gates or delete it, ` +
          `or agree a later date with its owner.`,
      ).toBe(true);
    },
  );
});

describe("reading a flag", () => {
  it("gives the flag's default when nothing is chosen on this device", () => {
    const demo = FLAGS.find((f) => f.id === DEMO)!;
    expect(flag(DEMO)).toBe(demo.default);
    const { result } = renderHook(() => useFlag(DEMO));
    expect(result.current).toBe(demo.default);
  });

  it("follows the choice made on this device, and its default again after it", () => {
    const { result } = renderHook(() => useFlag(DEMO));
    act(() => preferences.setFlag(DEMO, "on"));
    expect(result.current).toBe(true);
    expect(flag(DEMO)).toBe(true);
    act(() => preferences.setFlag(DEMO, "off"));
    expect(result.current).toBe(false);
    expect(flag(DEMO)).toBe(false);
    act(() => preferences.setFlag(DEMO, "on"));
    act(() => preferences.setFlag(DEMO, "default"));
    expect(result.current).toBe(false);
  });

  it("goes back to every default when the flags are reset", () => {
    preferences.setFlag(DEMO, "on");
    preferences.resetFlags();
    expect(flag(DEMO)).toBe(false);
  });

  it("can be set by a test as the app launched with it", () => {
    withFlags({ [DEMO]: true });
    expect(flag(DEMO)).toBe(true);
    resetPreferences();
    expect(flag(DEMO)).toBe(false);
  });
});

describe("flags read back from storage", () => {
  const defaults: Preferences = {
    theme: "light",
    nav: "hover",
    zoom: 1,
    sentTurn: "follow",
    followUp: "steer",
    pinnedSessions: [],
    flags: {},
    navLayout: { order: [], shown: {} },
  };

  it("takes the defaults from preferences saved before there were flags", () => {
    expect(restorePreferences({ theme: "dark" }, defaults).flags).toEqual({});
    expect(restorePreferences({ flags: "on" }, defaults).flags).toEqual({});
  });

  it("ignores ids this build does not know and settings that are not on or off", () => {
    expect(
      restorePreferences(
        { flags: { [DEMO]: "on", "gone-feature": "on", other: "maybe" } },
        defaults,
      ).flags,
    ).toEqual({ [DEMO]: "on" });
    expect(restorePreferences({ flags: { [DEMO]: true } }, defaults).flags).toEqual({});
  });

  it("drops an unknown id from storage on the next write", async () => {
    const stop = followOtherWindows();
    localStorage.setItem(
      "gents-preferences",
      JSON.stringify({
        state: { flags: { "gone-feature": "on", [DEMO]: "on" } },
        version: 1,
      }),
    );
    window.dispatchEvent(new StorageEvent("storage", { key: "gents-preferences" }));
    await waitFor(() => expect(flag(DEMO)).toBe(true));
    preferences.setFlag(DEMO, "off");
    const saved = JSON.parse(localStorage.getItem("gents-preferences")!) as {
      state: Preferences;
    };
    expect(saved.state.flags).toEqual({ [DEMO]: "off" });
    stop();
    localStorage.removeItem("gents-preferences");
  });
});
