/* The surfaces this app registers; importing this module fills the registry. */
import { ListTree, Users } from "lucide-react";
import { registerSurface } from "@/app/surfaces";
import { workersOfId } from "@/hooks/useFleet";
import { TraceSurface } from "./TracePanel";
import { WorkersSurface } from "./WorkersSurface";

registerSurface({
  id: "trace",
  title: "Trace",
  icon: ListTree,
  placements: ["dock", "sheet"],
  routes: ["session"],
  render: TraceSurface,
});

registerSurface({
  id: "workers",
  title: "Started sessions",
  icon: Users,
  placements: ["dock", "sheet"],
  routes: ["session"],
  render: WorkersSurface,
  badge: (fleet, sessionId) => workersOfId(fleet, sessionId).length || null,
});
