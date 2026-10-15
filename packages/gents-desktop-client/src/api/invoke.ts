import { normalizeInvokeError } from "../errors.js";
import type { DesktopCommands } from "../generated/DesktopCommands.js";
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
  return async function invokeDesktop<C extends keyof DesktopCommands>(
    command: C,
    ...[args]: {} extends DesktopCommands[C]["args"]
      ? [args?: DesktopCommands[C]["args"]]
      : [args: DesktopCommands[C]["args"]]
  ): Promise<DesktopCommands[C]["result"]> {
    if (requireTauriBridge && !hasTauriInvokeBridge()) {
      throw new Error(
        "Desktop native bridge is unavailable. Open this screen in the Tauri desktop app to save agent connections.",
      );
    }

    try {
      return await transport.invoke<DesktopCommands[C]["result"]>(
        command,
        args,
      );
    } catch (error) {
      throw normalizeInvokeError(error);
    }
  };
}
