import { useApp } from "@/app/AppContext";
import { nodeIsUnknown } from "../../hooks/fleetStore";
import { useFleet } from "../hooks/useFleet";
import { NewSession } from "./session/NewSession";
import { OpenSession } from "./session/OpenSession";
import { SessionOnUnknownNode } from "./session/SessionUnavailable";

/** `nodeDid`, `sessionId`: what the route names, which a link may name without
    this app knowing the node. */
export function SessionScreen({
  nodeDid,
  sessionId = null,
}: {
  nodeDid?: string;
  sessionId?: string | null;
}) {
  const selectedSessionId = useApp().stores.selection.use.sessionId();
  const unknownNode = useFleet((fleet) => nodeIsUnknown(fleet, nodeDid));
  if (unknownNode && nodeDid)
    return <SessionOnUnknownNode sessionId={sessionId} nodeDid={nodeDid} />;
  return selectedSessionId ? <OpenSession /> : <NewSession />;
}
