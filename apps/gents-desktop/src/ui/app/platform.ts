/* What the window does for itself, apart from any screen: its theme and
   platform styling, links that leave the app, the simulator's end-to-end
   boot, and the title macOS shows. */
import { useEffect } from "react";

import { installExternalLinkGuard } from "../../lib/externalLinks";
import {
  inNativeShell,
  onZoomMenu,
  setWebviewZoom,
  setWindowTitle,
} from "../../lib/nativeShell";
import { startNativeSimulatorE2e } from "../../lib/nativeSimulatorE2e";
import { applyShellPlatform, isMacTauriShell } from "../../lib/shellPlatform";
import { useSelectedSessionValue } from "../hooks/useSelectedSession";
import { useSelectedNode } from "../hooks/useClient";
import type { Route } from "../lib/router";
import { useApp } from "./AppContext";
import {
  applyChosenTheme,
  followOtherWindows,
  followZoom,
  preferences,
  zoomLevel,
} from "../preferences";
import { stepZoom } from "../lib/zoom";

/** The window's theme, platform styling, link guard and simulator boot, once. */
export function usePlatformSetup() {
  const { openExternalUrl } = useApp().actions;
  useEffect(() => {
    applyChosenTheme();
    applyShellPlatform();
  }, []);
  useEffect(
    () => installExternalLinkGuard(document, openExternalUrl),
    [openExternalUrl],
  );
  useEffect(() => {
    void startNativeSimulatorE2e();
  }, []);
  useEffect(() => followOtherWindows(), []);
  useEffect(() => {
    if (!inNativeShell()) return;
    const stopFollowing = followZoom(setWebviewZoom);
    const stopListening = onZoomMenu((action) =>
      preferences.setZoom(stepZoom(zoomLevel(), action)),
    );
    return () => {
      stopFollowing();
      stopListening();
    };
  }, []);
}

const TITLES: Partial<Record<Route["name"], string>> = {
  sessions: "Sessions",
  agents: "Nodes",
  mailbox: "Mailbox",
};

/** The macOS window title: the screen, then the selected node. */
export function useWindowTitle(route: Route) {
  const agent = useSelectedNode()?.node.displayName ?? null;
  const sessionTitle = useSelectedSessionValue((s) => s?.title ?? null);
  useEffect(() => {
    if (!isMacTauriShell()) return;
    const title =
      route.name === "session"
        ? sessionTitle || "New Session"
        : (TITLES[route.name] ?? "Configuration");
    setWindowTitle(agent ? `${title} — ${agent}` : `${title} — Gents`);
  }, [agent, route.name, sessionTitle]);
}
