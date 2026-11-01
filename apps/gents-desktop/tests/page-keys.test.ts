import { afterEach, expect, it, vi } from "vitest";

afterEach(() => {
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  vi.resetModules();
});

it("names the keys that go to a page in the desktop app", async () => {
  (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
  vi.resetModules();
  const { PAGE_KEYS, MOD } = await import("../src/ui/screens/Shortcuts");
  expect(PAGE_KEYS).toEqual({
    nodes: `${MOD}1`,
    sessions: `${MOD}2`,
    newSession: `${MOD}N`,
  });
});

it("names none in a browser, which keeps those keys", async () => {
  vi.resetModules();
  const { PAGE_KEYS } = await import("../src/ui/screens/Shortcuts");
  expect(Object.values(PAGE_KEYS).every((keys) => keys === undefined)).toBe(true);
});
