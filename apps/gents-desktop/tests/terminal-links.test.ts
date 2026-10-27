import { afterEach, expect, it, vi } from "vitest";
import type { ITerminalOptions } from "@xterm/xterm";
import { openTerminal } from "../src/ui/screens/terminal";

const fixture = vi.hoisted(() => ({
  options: null as ITerminalOptions | null,
  webLink: null as ((event: MouseEvent, uri: string) => void) | null,
}));
vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    options: ITerminalOptions;
    constructor(options: ITerminalOptions) {
      this.options = options;
      fixture.options = options;
    }
    loadAddon() {}
    open() {}
    dispose() {}
  },
}));
vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit() {}
  },
}));
vi.mock("@xterm/addon-search", () => ({ SearchAddon: class {} }));
vi.mock("@xterm/addon-web-links", () => ({
  WebLinksAddon: class {
    constructor(activate: (event: MouseEvent, uri: string) => void) {
      fixture.webLink = activate;
    }
  },
}));

afterEach(() => vi.restoreAllMocks());

it("routes OSC 8 and detected links through the same external scheme allowlist", async () => {
  const onLink = vi.fn();
  const confirm = vi.spyOn(window, "confirm");
  const open = vi.spyOn(window, "open");
  const terminal = await openTerminal(document.createElement("div"), { onLink });
  const event = new MouseEvent("click");
  const range = { start: { x: 1, y: 1 }, end: { x: 2, y: 1 } };
  for (const activate of [fixture.options!.linkHandler!.activate, fixture.webLink!]) {
    onLink.mockClear();
    for (const uri of [
      "javascript:alert(1)",
      "file:///etc/passwd",
      "data:text/html,hi",
      "tauri://localhost",
      "relative/path",
      "//example.test",
      "",
    ])
      activate(event, uri, range);
    expect(onLink).not.toHaveBeenCalled();
    for (const uri of [
      "https://example.test/path",
      "http://example.test",
      "mailto:test@example.test",
    ]) {
      activate(event, uri, range);
      expect(onLink).toHaveBeenLastCalledWith(uri);
    }
    expect(onLink).toHaveBeenCalledTimes(3);
  }
  expect(open).not.toHaveBeenCalled();
  expect(confirm).not.toHaveBeenCalled();
  terminal.dispose();
});
