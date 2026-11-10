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
  expect(
    await confirmPluginMessage("Task Board", "One line\nExact second line ✓"),
  ).toBe(true);
  expect(invoke).toHaveBeenCalledWith(
    "plugin:dialog|message",
    {
      title: "Message from Task Board",
      message:
        "Task Board wants to send this message to the selected chat:\n\nOne line\nExact second line ✓",
      kind: undefined,
      buttons: { OkCancelCustom: ["Send message", "Cancel"] },
    },
    undefined,
  );
  invoke.mockResolvedValue("Cancel");
  expect(await confirmPluginMessage("Task Board", "declined")).toBe(false);
  invoke.mockRejectedValue(new Error("dialog unavailable"));
  await expect(confirmPluginMessage("Task Board", "not approved")).rejects.toThrow(
    "dialog unavailable",
  );
});

it("names the plugin in the browser's confirm outside the native shell", async () => {
  const confirm = vi.spyOn(window, "confirm").mockReturnValue(false);
  expect(await confirmPluginMessage("Task Board", "proposed text")).toBe(false);
  expect(confirm).toHaveBeenCalledWith(
    "Task Board wants to send this message to the selected chat:\n\nproposed text",
  );
  confirm.mockRestore();
});
