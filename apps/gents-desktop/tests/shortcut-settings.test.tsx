import { fireEvent } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

const navigate = vi.hoisted(() => vi.fn());
vi.mock("@/lib/router", async (actual) => ({
  ...(await actual<typeof import("../src/ui/lib/router")>()),
  navigate,
}));

afterEach(() => {
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  vi.resetModules();
});

it("opens Settings on ⌘, in the desktop app", async () => {
  (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
  vi.resetModules();
  /* after the reset, so the screen and the app share one module graph */
  const { Shortcuts } = await import("../src/ui/screens/Shortcuts");
  const { renderIn, testApp } = await import("./app-fixture");
  renderIn(testApp(), <Shortcuts />);
  fireEvent.keyDown(window, { key: ",", metaKey: true });
  expect(navigate).toHaveBeenCalledWith({ name: "settings" });
}, 20_000);
