import { toast } from "sonner";
import type { FleetStore } from "./fleetStore";

/** Tells the person, once and quietly, when a node did not keep a change they
    made; the words are the runtime's own. */
export function noticeUnkeptChanges(fleet: FleetStore) {
  const told = new Set<string>();
  const tell = () => {
    for (const unkept of fleet.getState().unkeptChanges) {
      const key = `${unkept.nodeDid}\u0000${unkept.at}`;
      if (told.has(key)) continue;
      told.add(key);
      toast(unkept.label, {
        id: `unkept-change:${unkept.nodeDid}`,
        description: unkept.message,
      });
    }
  };
  fleet.subscribe(tell);
  tell();
}
