import { normalizeInvokeError } from "../errors.js";
import type { DesktopTransport } from "../transport.js";

type TauriInternalsWindow = Window & {
  __TAURI_INTERNALS__?: {
    invoke?: unknown;
  };
};

function hasTauriInvokeBridge() {
  return (
    typeof window !== "undefined" &&
    typeof (window as TauriInternalsWindow).__TAURI_INTERNALS__?.invoke ===
      "function"
  );
}

export function createDesktopInvoker(
  transport: DesktopTransport,
  requireTauriBridge = false,
) {
  return async function invokeDesktop<T>(
    command: string,
    args?: Record<string, unknown>,
  ): Promise<T> {
    if (requireTauriBridge && !hasTauriInvokeBridge()) {
      throw new Error(
        "Desktop native bridge is unavailable. Open this screen in the Tauri desktop app to save agent connections.",
      );
    }

    try {
      return await transport.invoke<T>(command, args);
    } catch (error) {
      throw normalizeInvokeError(error);
    }
  };
}
