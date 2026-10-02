/* The surfaces this app registers. Importing this module once (App does)
   fills the registry; a new surface is a file here and one entry. */
import { ListTree } from "lucide-react";
import { registerSurface } from "@/app/surfaces";
import { TraceSurface } from "./TracePanel";

registerSurface({
  id: "trace",
  title: "Trace",
  icon: ListTree,
  placements: ["dock", "sheet"],
  routes: ["session"],
  render: TraceSurface,
});
