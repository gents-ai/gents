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
