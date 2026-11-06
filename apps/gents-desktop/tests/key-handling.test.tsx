import { fireEvent, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const navigate = vi.hoisted(() => vi.fn());
const openSearch = vi.hoisted(() => vi.fn());
vi.mock("@/lib/router", async (actual) => ({
  ...(await actual<typeof import("../src/ui/lib/router")>()),
  navigate,
}));
/* the palette registers its own command; this stand-in does the same */
vi.mock("@/screens/SearchPalette", async () => {
  const { registerCommand } = await import("@/commands/commands");
  registerCommand({
    id: "search",
    title: "Search",
    group: "Help",
    keys: { key: "k", mod: true },
    run: () => openSearch(),
  });
  return { openSearch };
});

import { useHistoryInputs } from "../src/ui/lib/history-inputs";
import { Shortcuts } from "../src/ui/screens/Shortcuts";
import { commandKey, IS_MAC, keyTaken } from "../src/ui/screens/shortcutKeys";
import { renderIn, testApp } from "./app-fixture";

const mod = IS_MAC ? "metaKey" : "ctrlKey";
const other = IS_MAC ? "ctrlKey" : "metaKey";

afterEach(() => {
  document.body.innerHTML = "";
  navigate.mockClear();
  openSearch.mockClear();
});

describe("the app's command key", () => {
  it("is ⌘ alone on a Mac, so ⌃ keys stay with text editing", () => {
    expect(commandKey({ metaKey: true, ctrlKey: false }, true)).toBe(true);
    expect(commandKey({ metaKey: false, ctrlKey: true }, true)).toBe(false);
    expect(commandKey({ metaKey: true, ctrlKey: true }, true)).toBe(false);
  });

  it("is Ctrl alone elsewhere", () => {
    expect(commandKey({ metaKey: false, ctrlKey: true }, false)).toBe(true);
    expect(commandKey({ metaKey: true, ctrlKey: false }, false)).toBe(false);
    expect(commandKey({ metaKey: true, ctrlKey: true }, false)).toBe(false);
  });

  it("opens search with the platform's key and leaves the other alone", () => {
    renderIn(testApp(), <Shortcuts />);
    fireEvent.keyDown(window, { key: "k", [other]: true });
    expect(openSearch).not.toHaveBeenCalled();
    fireEvent.keyDown(window, { key: "k", [mod]: true });
    expect(openSearch).toHaveBeenCalledTimes(1);
  });
});

describe("focusing the composer", () => {
  /* ⌘L is the desktop app's: a browser keeps it for its address bar */
  const desktopShortcuts = async () => {
    (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
    vi.resetModules();
    const { Shortcuts } = await import("../src/ui/screens/Shortcuts");
    const fixture = await import("./app-fixture");
    fixture.renderIn(fixture.testApp(), <Shortcuts />);
  };
  afterEach(() => {
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    vi.resetModules();
  });
  const pressL = () => {
    const key = new KeyboardEvent("keydown", {
      key: "l",
      [mod]: true,
      cancelable: true,
    });
    window.dispatchEvent(key);
    return key;
  };

  it("focuses the composer's editable field, not a text box elsewhere", async () => {
    await desktopShortcuts();
    /* jsdom focuses an editable element only with a tab index */
    document.body.insertAdjacentHTML(
      "beforeend",
      `<textarea aria-label="Elsewhere"></textarea>
       <div data-slot="composer" hidden><div contenteditable="true" aria-label="Hidden"></div></div>
       <div data-slot="composer"><div contenteditable="true" tabindex="0" aria-label="Message"></div></div>`,
    );
    expect(pressL().defaultPrevented).toBe(true);
    expect(document.activeElement?.getAttribute("aria-label")).toBe("Message");
  }, 20_000);

  it("leaves the key alone where there is no composer", async () => {
    await desktopShortcuts();
    document.body.insertAdjacentHTML("beforeend", "<textarea></textarea>");
    expect(pressL().defaultPrevented).toBe(false);
    expect(document.activeElement).toBe(document.body);
  }, 20_000);
});

describe("keys a control already took", () => {
  it("counts a key pressed in a tab list or already handled as taken", () => {
    document.body.innerHTML = `<div role="tablist"><button id="tab">A</button></div><p id="text">x</p>`;
    const inTabs = new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true });
    document.getElementById("tab")!.dispatchEvent(inTabs);
    expect(keyTaken(inTabs)).toBe(true);
    const handled = new KeyboardEvent("keydown", {
      key: "ArrowLeft",
      bubbles: true,
      cancelable: true,
    });
    handled.preventDefault();
    document.getElementById("text")!.dispatchEvent(handled);
    expect(keyTaken(handled)).toBe(true);
  });

  it("does not go back in history on Alt+← in a tab list", () => {
    Object.defineProperty(navigator, "platform", {
      value: "Win32",
      configurable: true,
    });
    const h = { canBack: true, canForward: true, back: vi.fn(), forward: vi.fn() };
    renderHook(() => useHistoryInputs(h));
    document.body.innerHTML = `<div role="tablist"><button id="tab">A</button></div>`;
    document
      .getElementById("tab")!
      .dispatchEvent(
        new KeyboardEvent("keydown", { key: "ArrowLeft", altKey: true, bubbles: true }),
      );
    expect(h.back).not.toHaveBeenCalled();
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowLeft", altKey: true }),
    );
    expect(h.back).toHaveBeenCalledTimes(1);
    Object.defineProperty(navigator, "platform", { value: "", configurable: true });
  });
});
