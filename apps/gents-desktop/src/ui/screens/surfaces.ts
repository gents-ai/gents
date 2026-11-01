import { FileDiff, SquareStack, Terminal } from "lucide-react";
import { registerSurface } from "@/app/surfaces";
import { startedUnderId } from "@/hooks/useFleet";
import { BackgroundSurface } from "./BackgroundCalls";
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
