/* The surfaces this app registers. Importing this module once (App does)
   fills the registry; a new surface is a file here and one entry. */
import { ListTree, Users } from "lucide-react";
import { registerSurface } from "@/app/surfaces";
import { workersBySession } from "@/lib/nodes";
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
  title: "Workers",
  icon: Users,
  placements: ["dock", "sheet"],
  routes: ["session"],
  render: WorkersSurface,
  badge: (nodes, sessionId) =>
    sessionId ? (workersBySession(nodes).get(sessionId)?.length ?? 0) || null : null,
});
