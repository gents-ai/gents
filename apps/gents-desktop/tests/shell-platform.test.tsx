import { afterEach, describe, expect, it, vi } from "vitest";

import {
  applyShellPlatform,
  headerIsWindowBar,
  isLinuxTauriShell,
  isMacTauriShell,
  isMobileTauriShell,
  isWindowsTauriShell,
  ownsAutomaticRecovery,
} from "../src/lib/shellPlatform";

const windowMocks = vi.hoisted(() => ({
  label: "main",
  isFullscreen: vi.fn().mockResolvedValue(false),
  onResized: vi.fn().mockResolvedValue(vi.fn()),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => windowMocks,
}));

const originalPlatform = navigator.platform;
const originalUserAgent = navigator.userAgent;
const originalMaxTouchPoints = navigator.maxTouchPoints;

function enterTauri(platform: string, userAgent = originalUserAgent, touchPoints = 0) {
  (window as Record<string, unknown>).__TAURI_INTERNALS__ = {};
  Object.defineProperty(navigator, "platform", { configurable: true, value: platform });
  Object.defineProperty(navigator, "userAgent", {
    configurable: true,
    value: userAgent,
  });
  Object.defineProperty(navigator, "maxTouchPoints", {
    configurable: true,
    value: touchPoints,
  });
}

describe("native shell classifier", () => {
  afterEach(() => {
    delete (window as Record<string, unknown>).__TAURI_INTERNALS__;
    delete document.documentElement.dataset.shell;
    delete document.documentElement.dataset.windowFullscreen;
    Object.defineProperty(navigator, "platform", {
      configurable: true,
      value: originalPlatform,
    });
    Object.defineProperty(navigator, "userAgent", {
      configurable: true,
      value: originalUserAgent,
    });
    Object.defineProperty(navigator, "maxTouchPoints", {
      configurable: true,
      value: originalMaxTouchPoints,
    });
    vi.clearAllMocks();
    windowMocks.label = "main";
  });

  it("does not stamp a browser shell", () => {
    applyShellPlatform();

    expect(document.documentElement.dataset.shell).toBeUndefined();
    expect(headerIsWindowBar()).toBe(false);
  });

  it("keeps automatic shared-client recovery in the original native view", () => {
    expect(ownsAutomaticRecovery()).toBe(true);
    enterTauri("MacIntel");
    expect(ownsAutomaticRecovery()).toBe(true);
    windowMocks.label = "gents-view-1";
    expect(ownsAutomaticRecovery()).toBe(false);
    enterTauri("iPhone", "iPhone", 5);
    expect(ownsAutomaticRecovery()).toBe(true);
  });

  it("lets macOS own title and tab chrome outside the web viewport", () => {
    enterTauri("MacIntel");

    applyShellPlatform();

    expect(isMacTauriShell()).toBe(true);
    expect(headerIsWindowBar()).toBe(false);
    expect(document.documentElement.dataset.shell).toBe("mac");
    expect(windowMocks.onResized).not.toHaveBeenCalled();
    expect(windowMocks.isFullscreen).not.toHaveBeenCalled();
  });

  it("classifies Windows as a custom window bar", () => {
    enterTauri("Win32");

    applyShellPlatform();

    expect(isWindowsTauriShell()).toBe(true);
    expect(headerIsWindowBar()).toBe(true);
    expect(document.documentElement.dataset.shell).toBe("windows");
  });

  it("classifies Linux without replacing its native window bar", () => {
    enterTauri("Linux x86_64");

    applyShellPlatform();

    expect(isLinuxTauriShell()).toBe(true);
    expect(headerIsWindowBar()).toBe(false);
    expect(document.documentElement.dataset.shell).toBe("linux");
  });

  it("does not mistake a touch-capable iPad for macOS", () => {
    enterTauri("MacIntel", "Mozilla/5.0 (iPad; CPU OS 26_5 like Mac OS X)", 5);

    applyShellPlatform();

    expect(isMobileTauriShell()).toBe(true);
    expect(isMacTauriShell()).toBe(false);
    expect(headerIsWindowBar()).toBe(false);
    expect(document.documentElement.dataset.shell).toBeUndefined();
  });

  it("classifies mobile only inside a mobile Tauri shell", () => {
    Object.defineProperty(navigator, "userAgent", {
      configurable: true,
      value: "Mozilla/5.0 (iPhone; CPU iPhone OS 26_5 like Mac OS X)",
    });
    expect(isMobileTauriShell()).toBe(false);

    (window as Record<string, unknown>).__TAURI_INTERNALS__ = {};
    expect(isMobileTauriShell()).toBe(true);
  });
});
