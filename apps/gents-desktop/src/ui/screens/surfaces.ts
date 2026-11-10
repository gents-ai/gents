import { FileDiff, Globe, SquareStack, Terminal } from "lucide-react";
import { registerSurface } from "@/app/surfaces";
import { flag } from "@/preferences";
import { isMacTauriShell } from "../../lib/shellPlatform";
import { startedUnderId } from "@/hooks/useFleet";
import { BackgroundSurface } from "./BackgroundCalls";
import { BrowserSurface, resetBrowser } from "./browser/BrowserSurface";
import { ChangesSurface } from "./ChangesSurface";
import { startedCount } from "./StartedCount";
import { WorkersSurface } from "./WorkersSurface";

registerSurface({
  id: "workers",
  title: "Started sessions",
  tab: "Started",
  icon: SquareStack,
  placements: ["dock", "sheet"],
  routes: ["session"],
  render: WorkersSurface,
  badge: (fleet, sessionId) => {
    const counts = startedUnderId(fleet, sessionId);
    return counts.started === 0
      ? null
      : {
          count: counts.started,
          label: startedCount(counts),
          mark: counts.failed > 0 ? "alert" : counts.running > 0 ? "active" : null,
        };
  },
});

registerSurface({
  id: "background",
  title: "Background",
  icon: Terminal,
  placements: ["dock", "sheet"],
  routes: ["session"],
  render: BackgroundSurface,
});

registerSurface({
  id: "changes",
  title: "Changes",
  icon: FileDiff,
  placements: ["dock", "sheet"],
  routes: ["session"],
  render: ChangesSurface,
});

/* the page is a native view only the macOS app has; the flag applies at
   launch, so the panel is there for this run or not at all */
if (flag("browser") && isMacTauriShell()) {
  resetBrowser();
  registerSurface({
    id: "browser",
    title: "Browser",
    icon: Globe,
    placements: ["dock"],
    render: BrowserSurface,
  });
}
