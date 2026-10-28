import { afterEach, expect, it, vi } from "vitest";
import { confirmPluginMessage } from "../src/lib/nativeShell";

afterEach(() => {
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
});

it("uses the native dialog message command with the exact proposed text and explicit send/cancel labels", async () => {
  const invoke = vi.fn().mockResolvedValue("Send message");
  Object.defineProperty(window, "__TAURI_INTERNALS__", {
    configurable: true,
    value: { invoke },
  });
  expect(await confirmPluginMessage("One line\nExact second line ✓")).toBe(true);
  expect(invoke).toHaveBeenCalledWith(
    "plugin:dialog|message",
    {
      title: "Plugin message",
      message:
        "Send this message to the selected chat?\n\nOne line\nExact second line ✓",
      kind: undefined,
      buttons: { OkCancelCustom: ["Send message", "Cancel"] },
    },
    undefined,
  );
  invoke.mockResolvedValue("Cancel");
  expect(await confirmPluginMessage("declined")).toBe(false);
  invoke.mockRejectedValue(new Error("dialog unavailable"));
  await expect(confirmPluginMessage("not approved")).rejects.toThrow(
    "dialog unavailable",
  );
});
