import { afterEach, describe, expect, it } from "vitest";

import { applyShellPlatform, isMobileTauriShell } from "../src/lib/shellPlatform";

const originalPlatform = navigator.platform;
const originalUserAgent = navigator.userAgent;

describe("macOS shell classifier", () => {
  afterEach(() => {
    delete (window as Record<string, unknown>).__TAURI_INTERNALS__;
    delete document.documentElement.dataset.shell;
    Object.defineProperty(navigator, "platform", {
      configurable: true,
      value: originalPlatform,
    });
    Object.defineProperty(navigator, "userAgent", {
      configurable: true,
      value: originalUserAgent,
    });
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

  it("classifies mobile only inside a mobile Tauri shell", () => {
    Object.defineProperty(navigator, "userAgent", {
      configurable: true,
      value: "Mozilla/5.0 (iPhone; CPU iPhone OS 26_5 like Mac OS X)",
    });
    expect(isMobileTauriShell()).toBe(false);

    (window as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    expect(isMobileTauriShell()).toBe(true);

    Object.defineProperty(navigator, "userAgent", {
      configurable: true,
      value: "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_0)",
    });
    expect(isMobileTauriShell()).toBe(false);
  });
});
