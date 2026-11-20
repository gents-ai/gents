/* A pack reinstalled from the CLI announces nothing to an open window, so
   the window rescans its UX plugins when the native window regains focus.
   A shell with no local home (mobile) has no packs to reinstall and never
   listens. */
import { afterEach, expect, it, vi } from "vitest";

import { resetRuntimeDoor } from "@/contrib/runtime-door";
import { resetUxDoors, useUxHost } from "@/hooks/useUxHost";
import { renderRouted, testApp } from "./app-fixture";

const shell = vi.hoisted(() => ({
  localHome: true,
  focused: null as (() => void) | null,
  stop: vi.fn(),
  rescan: vi.fn(),
}));

vi.mock("../src/lib/nativeShell", async (actual) => ({
  ...(await actual<typeof import("../src/lib/nativeShell")>()),
  inNativeShell: () => true,
  onWindowFocused: (handler: () => void) => {
    shell.focused = handler;
    return shell.stop;
  },
}));
vi.mock("../src/lib/shellPlatform", async (actual) => ({
  ...(await actual<typeof import("../src/lib/shellPlatform")>()),
  supportsLocalManagedServer: () => shell.localHome,
}));
vi.mock("@/contrib/bundled", () => ({ discoverBundledUxPlugins: () => undefined }));
vi.mock("@/contrib/runtime-door", async (actual) => ({
  ...(await actual<typeof import("../src/ui/contrib/runtime-door")>()),
  rescanRuntimeUxPluginsSoon: shell.rescan,
}));

afterEach(() => {
  resetUxDoors();
  resetRuntimeDoor();
  shell.localHome = true;
  shell.focused = null;
  shell.stop.mockClear();
  shell.rescan.mockClear();
});

function mountHost() {
  const app = testApp();
  function Host() {
    useUxHost(app, async () => () => undefined);
    return null;
  }
  return renderRouted(app, { name: "agents" }, () => <Host />);
}

it("rescans when the native window regains focus, and stops listening on unmount", () => {
  const view = mountHost();
  expect(shell.rescan).not.toHaveBeenCalled();

  shell.focused?.();
  expect(shell.rescan).toHaveBeenCalledOnce();

  view.unmount();
  expect(shell.stop).toHaveBeenCalledOnce();
});

it("never listens for focus on a shell without a local home", () => {
  shell.localHome = false;
  mountHost();
  expect(shell.focused).toBeNull();
});
