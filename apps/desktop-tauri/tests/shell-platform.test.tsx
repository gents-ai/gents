import { afterEach, describe, expect, it } from "vitest";

import { applyShellPlatform } from "../src/lib/shellPlatform";

describe("macOS shell classifier", () => {
  afterEach(() => {
    delete (window as Record<string, unknown>).__TAURI_INTERNALS__;
    delete document.documentElement.dataset.shell;
  });

  it("stamps the shell only inside the macOS Tauri app", () => {
    applyShellPlatform();
    expect(document.documentElement.dataset.shell).toBeUndefined();

    (window as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    Object.defineProperty(navigator, "platform", {
      configurable: true,
      value: "MacIntel",
    });
    applyShellPlatform();
    expect(document.documentElement.dataset.shell).toBe("mac");
  });
});
