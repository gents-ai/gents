import { toast } from "sonner";
import { currentRoute, navigate } from "../ui/lib/router";
import type { FleetStore } from "./fleetStore";
import { endedAccessDetail, endedAccessTitle } from "./nodeAccess";

/* records older than this run were ended while the app was closed and are
   found again at launch; only the Nodes list shows those */
const SAME_RUN_SLACK_MS = 60_000;

/** Tells the person, once, when a node this app joined stops letting it in,
    so the node doesn't simply vanish from the menus. */
export function noticeEndedAccess(fleet: FleetStore, startedAt = Date.now()) {
  const told = new Set<string>();
  const tell = () => {
    for (const ended of fleet.getState().endedAccess) {
      const key = `${ended.nodeDid}\u0000${ended.endedAt}`;
      if (told.has(key)) continue;
      told.add(key);
      if (Date.parse(ended.endedAt) < startedAt - SAME_RUN_SLACK_MS) continue;
      const onNodes = ["nodes", "agents"].includes(currentRoute()?.name ?? "");
      toast(endedAccessTitle(ended), {
        id: `ended-access:${ended.nodeDid}`,
        description: endedAccessDetail(ended),
        duration: Infinity,
        /* the Nodes page already shows it, so the way there would go nowhere */
        ...(onNodes
          ? {}
          : { action: { label: "Nodes", onClick: () => navigate({ name: "nodes" }) } }),
      });
    }
  };
  fleet.subscribe(tell);
  tell();
}
