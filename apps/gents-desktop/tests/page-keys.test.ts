import { afterEach, expect, it, vi } from "vitest";

afterEach(() => {
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  vi.resetModules();
});

const PAGES = ["page.nodes", "page.sessions", "page.new-session", "page.settings"];

it("names the keys that go to a page in the desktop app", async () => {
  (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
  vi.resetModules();
  await import("../src/ui/commands/app");
  const { keysHint } = await import("../src/ui/commands/commands");
  const { MOD } = await import("../src/ui/screens/shortcutKeys");
  expect(PAGES.map(keysHint)).toEqual([`${MOD}1`, `${MOD}2`, `${MOD}N`, `${MOD},`]);
});

it("names none in a browser, which keeps those keys", async () => {
  vi.resetModules();
  await import("../src/ui/commands/app");
  const { keysHint } = await import("../src/ui/commands/commands");
  expect(PAGES.map(keysHint).every((keys) => keys === undefined)).toBe(true);
});
