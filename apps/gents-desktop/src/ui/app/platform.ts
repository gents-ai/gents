/* What the window does for itself, apart from any screen: its theme and
   platform styling, links that leave the app, the simulator's end-to-end
   boot, and the title macOS shows. */
import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { installExternalLinkGuard } from "../../lib/externalLinks";
import { startNativeSimulatorE2e } from "../../lib/nativeSimulatorE2e";
import { applyShellPlatform, isMacTauriShell } from "../../lib/shellPlatform";
import { useSelectedSessionValue } from "../hooks/useSelectedSession";
import { useSelectedDeployment } from "../hooks/useClient";
import type { Route } from "../lib/router";
import { initTheme } from "../theme";

/** The window's theme, platform styling, link guard and simulator boot, once. */
export function usePlatformSetup() {
  useEffect(() => {
    initTheme();
    applyShellPlatform();
  }, []);
  useEffect(() => installExternalLinkGuard(document), []);
  useEffect(() => {
    void startNativeSimulatorE2e();
  }, []);
}

const TITLES: Partial<Record<Route["name"], string>> = {
  sessions: "Sessions",
  agents: "Agents",
  mailbox: "Mailbox",
};

/** The macOS window title: the screen, then the selected agent. */
export function useWindowTitle(route: Route) {
  const agent = useSelectedDeployment()?.agentPrincipal.displayName ?? null;
  const sessionTitle = useSelectedSessionValue((s) => s?.title ?? null);
  useEffect(() => {
    if (!isMacTauriShell()) return;
    const title =
      route.name === "session"
        ? sessionTitle || "New Session"
        : (TITLES[route.name] ?? "Configuration");
    void getCurrentWindow().setTitle(
      agent ? `${title} — ${agent}` : `${title} — Gents`,
    );
  }, [agent, route.name, sessionTitle]);
}
