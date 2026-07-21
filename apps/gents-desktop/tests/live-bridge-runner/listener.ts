import type { DesktopClientUpdatedListenerFactory } from "../../src/lib/desktop-events";
import type { RunnerExitStatus } from "./logs";

export const VERSION_POLL_MS = 250;

export function createVersionPollingListenerFactory({
  fetchVersion,
  getExitStatus,
  logError,
  pollMs = VERSION_POLL_MS,
}: {
  fetchVersion: () => Promise<number>;
  getExitStatus: () => RunnerExitStatus | null;
  logError: (message: string) => void;
  pollMs?: number;
}): DesktopClientUpdatedListenerFactory {
  return async (handler) => {
    let disposed = false;
    let inFlight = false;
    let lastVersion = 0;
    try {
      lastVersion = await fetchVersion();
    } catch (error) {
      if (!getExitStatus()) {
        logError(`[listener:init] ${String(error)}\n`);
      }
    }
    const timer = setInterval(async () => {
      if (disposed || inFlight) {
        return;
      }
      inFlight = true;
      try {
        const nextVersion = await fetchVersion();
        if (nextVersion !== lastVersion) {
          lastVersion = nextVersion;
          await handler({ reason: "store" });
        }
      } catch (error) {
        if (!disposed && !getExitStatus()) {
          logError(`[listener] ${String(error)}\n`);
        }
      } finally {
        inFlight = false;
      }
    }, pollMs);

    return () => {
      disposed = true;
      clearInterval(timer);
    };
  };
}
