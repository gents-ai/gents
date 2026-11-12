/* A plugin's own window while the app's client starts: what it is waiting
   for, and where to look if starting fails. Setup and recovery stay with the
   main window, so this offers none of their actions. */

import { AsciiLoader } from "@gents/ui/components/ascii-loader";
import type { DesktopStartupPhase } from "../../lib/loadingStatus";

const FAILED: readonly DesktopStartupPhase[] = [
  "managed-server-error",
  "configuration-error",
  "client-error",
];

export function PluginWindowWaiting({
  phase,
  title,
}: {
  phase: DesktopStartupPhase;
  /** the page's name, as the window is titled */
  title: string | null;
}) {
  const failed = FAILED.includes(phase);
  return (
    <section
      data-testid="plugin-window-waiting"
      data-failed={failed || undefined}
      className="grid h-dvh place-items-center bg-background px-8 text-foreground"
    >
      <div className="grid max-w-sm justify-items-center gap-2 text-center">
        {failed ? (
          <>
            <h1 className="font-heading text-base font-medium text-heading">
              This page can’t open yet
            </h1>
            <p className="text-sm text-muted-foreground">
              Gents couldn’t start. Its main window says why and how to fix it.
            </p>
          </>
        ) : (
          <p
            role="status"
            className="flex items-center gap-2 text-sm text-muted-foreground"
          >
            <AsciiLoader aria-hidden className="text-foreground" />
            {title ? `Opening ${title}…` : "Opening…"}
          </p>
        )}
      </div>
    </section>
  );
}
