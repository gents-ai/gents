import type { SharingDecision, SharingDecisionKind } from "../../../hooks/sharing";
import { fullTime, shortTime } from "../TranscriptMarker";
import { ItemRow, partyName, scopeText } from "./parts";

const DECIDED: Record<SharingDecisionKind, string> = {
  approved: "Approved",
  denied: "Denied",
  revoked: "Revoked",
  expired: "Expired",
  left: "Left",
};

/** One line of the node's sharing history: what happened to whom, for
    what, who decided it and when. */
export function DecisionRow({ decision }: { decision: SharingDecision }) {
  const who = partyName(decision.party);
  const when = shortTime(decision.at);
  return (
    <ItemRow
      inline
      label={`${DECIDED[decision.kind]}: ${who}`}
      title={
        <span className="truncate" title={decision.party.did}>
          {who}
        </span>
      }
      description={
        <>
          <span>{DECIDED[decision.kind]}</span>
          <span>{scopeText(decision.scope)}</span>
          {decision.by && <span>By {partyName(decision.by)}</span>}
        </>
      }
      actions={
        when && (
          <time
            dateTime={decision.at}
            title={fullTime(decision.at)}
            className="text-xs text-muted-foreground tabular-nums"
          >
            {when}
          </time>
        )
      }
    />
  );
}
