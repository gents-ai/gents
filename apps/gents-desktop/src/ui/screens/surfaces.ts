import { FileDiff, SquareStack, Terminal } from "lucide-react";
import { registerSurface } from "@/app/surfaces";
import { workersOfId } from "@/hooks/useFleet";
import { BackgroundSurface } from "./BackgroundCalls";
import { ChangesSurface } from "./ChangesSurface";
import { WorkersSurface } from "./WorkersSurface";

registerSurface({
  id: "workers",
  title: "Started sessions",
  icon: SquareStack,
  placements: ["dock", "sheet"],
  routes: ["session"],
  render: WorkersSurface,
  badge: (fleet, sessionId) => workersOfId(fleet, sessionId).length || null,
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
