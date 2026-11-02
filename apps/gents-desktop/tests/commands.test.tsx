import { act, fireEvent, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { registerCommand, type Command } from "../src/ui/commands/commands";
import { dispatchKey } from "../src/ui/commands/dispatch";
import { chordText, matches } from "../src/ui/commands/keys";
import { SearchPalette, openSearch } from "../src/ui/screens/SearchPalette";
import { Shortcuts } from "../src/ui/screens/Shortcuts";
import { renderIn, testApp } from "./app-fixture";

const press = (init: KeyboardEventInit & { key: string }, target?: Element) => {
  const e = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
  if (target) Object.defineProperty(e, "target", { value: target });
  return e;
};

const command = (over: Partial<Command> = {}): Command => ({
  id: "test.probe",
  title: "Probe",
  group: "Help",
  keys: { key: "p", mod: true },
  run: vi.fn(),
  ...over,
});

let off: (() => void)[] = [];
afterEach(() => {
  off.forEach((f) => f());
  off = [];
  added.forEach((el) => el.remove());
  added = [];
});

let added: Element[] = [];
const attach = <T extends Element>(el: T) => {
  document.body.append(el);
  added.push(el);
  return el;
};

describe("a chord", () => {
  it("on a Mac is ⌘ alone for the command key, so ⌃ keys stay with text editing", () => {
    const chord = { key: "k", mod: true };
    expect(matches(chord, press({ key: "k", metaKey: true }), true)).toBe(true);
    expect(matches(chord, press({ key: "k", ctrlKey: true }), true)).toBe(false);
    expect(
      matches(chord, press({ key: "k", metaKey: true, ctrlKey: true }), true),
    ).toBe(false);
    expect(chordText({ key: "j", mod: true, shift: true }, true)).toBe("⇧⌘J");
  });

  it("elsewhere is Ctrl alone, with its own keys where a platform needs them", () => {
    const chord = { key: "k", mod: true };
    expect(matches(chord, press({ key: "K", ctrlKey: true }), false)).toBe(true);
    expect(matches(chord, press({ key: "k", metaKey: true }), false)).toBe(false);
    expect(chordText({ key: "j", mod: true, shift: true }, false)).toBe("Ctrl+Shift+J");
    expect(chordText({ key: "ArrowUp", alt: true }, false)).toBe("Alt+↑");
  });
});

describe("dispatching a key", () => {
  it("runs the command it matches and takes the key from the page", () => {
    const probe = command();
    const e = press({ key: "p", ctrlKey: true });
    expect(dispatchKey(e, [probe], false)).toBe(true);
    expect(probe.run).toHaveBeenCalledWith(0);
    expect(e.defaultPrevented).toBe(true);
  });

  it("leaves a desktop-only key to a browser", () => {
    const probe = command({ desktopOnly: true });
    const e = press({ key: "p", ctrlKey: true });
    expect(dispatchKey(e, [probe], false)).toBe(false);
    expect(probe.run).not.toHaveBeenCalled();
    expect(e.defaultPrevented).toBe(false);
  });

  it("leaves a key taken already, or pressed in a control with keys of its own", () => {
    const probe = command({ yieldsToControls: true });
    const taken = press({ key: "p", ctrlKey: true });
    taken.preventDefault();
    expect(dispatchKey(taken, [probe], false)).toBe(false);
    const list = attach(document.createElement("div"));
    list.setAttribute("role", "listbox");
    expect(dispatchKey(press({ key: "p", ctrlKey: true }, list), [probe], false)).toBe(
      false,
    );
    expect(probe.run).not.toHaveBeenCalled();
  });

  it("leaves a key that is typing's to a text field", () => {
    const probe = command({ keys: { key: "ArrowUp", alt: true }, notInFields: true });
    const field = attach(document.createElement("textarea"));
    expect(
      dispatchKey(press({ key: "ArrowUp", altKey: true }, field), [probe], false),
    ).toBe(false);
    expect(dispatchKey(press({ key: "ArrowUp", altKey: true }), [probe], false)).toBe(
      true,
    );
  });

  it("goes on to the next command when one can't run here", () => {
    const idle = command({ id: "a", when: () => false });
    const declines = command({ id: "b", run: vi.fn(() => false) });
    const takes = command({ id: "c" });
    expect(
      dispatchKey(press({ key: "p", ctrlKey: true }), [idle, declines, takes], false),
    ).toBe(true);
    expect(idle.run).not.toHaveBeenCalled();
    expect(takes.run).toHaveBeenCalled();
  });

  it("tells a family's command which of its keys was pressed", () => {
    const nth = command({
      keys: [1, 2, 3].map((n) => ({ key: String(n), alt: true })),
    });
    dispatchKey(press({ key: "3", altKey: true }), [nth], false);
    expect(nth.run).toHaveBeenCalledWith(2);
  });
});

describe("the reference", () => {
  it("lists every command with keys, by group", () => {
    off.push(
      registerCommand(
        command({ id: "test.listed", title: "A listed probe", group: "Session" }),
      ),
    );
    renderIn(testApp(), <Shortcuts />);
    act(() => {
      fireEvent.keyDown(window, { key: "/", ctrlKey: true });
    });
    const dialog = screen.getByTestId("shortcuts-help");
    expect(dialog).toHaveTextContent("A listed probe");
    expect(dialog).toHaveTextContent("Ctrl+P");
    expect(dialog).toHaveTextContent("Show keyboard shortcuts");
    expect(dialog).toHaveTextContent("Session");
  });
});

describe("the search palette's actions", () => {
  it("list a registered command, and run it", async () => {
    const run = vi.fn();
    off.push(
      registerCommand(
        command({ id: "test.palette", title: "Reticulate splines", run }),
      ),
    );
    const { renderIn, testApp } = await import("./app-fixture");
    renderIn(testApp(), <SearchPalette />);
    act(() => openSearch());
    fireEvent.change(screen.getByRole("combobox", { name: "Search" }), {
      target: { value: "reticulate" },
    });
    fireEvent.click(await screen.findByRole("option", { name: /Reticulate splines/ }));
    expect(run).toHaveBeenCalled();
  });
});
