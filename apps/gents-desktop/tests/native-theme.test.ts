import { afterEach, beforeEach, expect, it, vi } from "vitest";

const native = vi.hoisted(() => ({ mac: true, setTheme: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ setTheme: native.setTheme }),
}));
vi.mock("../src/lib/shellPlatform", () => ({
  isMacTauriShell: () => native.mac,
}));
import { applyTheme } from "../src/ui/theme";

beforeEach(() => {
  native.mac = true;
  native.setTheme.mockReset().mockResolvedValue(undefined);
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
  });
});
afterEach(() => vi.unstubAllGlobals());

it("applies the saved theme to native macOS chrome on each view's startup", async () => {
  localStorage.setItem(
    "gents-preferences",
    JSON.stringify({ state: { theme: "dark" }, version: 1 }),
  );
  vi.resetModules();
  const { applyChosenTheme, preferences } = await import("../src/ui/preferences");
  applyChosenTheme();
  expect(native.setTheme).toHaveBeenCalledWith("dark");
  preferences.setTheme("light");
  expect(native.setTheme).toHaveBeenLastCalledWith("light");
  expect(document.documentElement.dataset.theme).toBeUndefined();
});

it("leaves other platforms' native appearance unchanged", () => {
  native.mac = false;
  applyTheme("dark");
  expect(native.setTheme).not.toHaveBeenCalled();
  expect(document.documentElement.dataset.theme).toBe("dark");
});

function systemAppearance(dark: boolean) {
  const state = { dark, listeners: [] as (() => void)[] };
  vi.stubGlobal("matchMedia", () => ({
    get matches() {
      return state.dark;
    },
    addEventListener: (_: string, listener: () => void) =>
      state.listeners.push(listener),
    removeEventListener: () => {},
  }));
  return {
    turn(next: boolean) {
      state.dark = next;
      for (const listener of state.listeners) listener();
    },
  };
}

it("follows the system's appearance by default, and as the system changes", async () => {
  const system = systemAppearance(false);
  vi.resetModules();
  const { applyChosenTheme } = await import("../src/ui/preferences");
  applyChosenTheme();
  /* the window is left to follow the system too */
  expect(native.setTheme).toHaveBeenLastCalledWith(null);
  expect(document.documentElement.dataset.theme).toBeUndefined();
  system.turn(true);
  expect(document.documentElement.dataset.theme).toBe("dark");
  system.turn(false);
  expect(document.documentElement.dataset.theme).toBeUndefined();
});

it("keeps a chosen theme whatever the system does", async () => {
  localStorage.setItem(
    "gents-preferences",
    JSON.stringify({ state: { theme: "light" }, version: 1 }),
  );
  const system = systemAppearance(false);
  vi.resetModules();
  const { applyChosenTheme } = await import("../src/ui/preferences");
  applyChosenTheme();
  system.turn(true);
  expect(document.documentElement.dataset.theme).toBeUndefined();
  expect(native.setTheme).toHaveBeenLastCalledWith("light");
});
