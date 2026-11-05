import { useThisAppDid } from "@/hooks/useClient";
import type {
  SharingDecision,
  SharingDecisionKind,
  SharingHistoryLimits,
} from "../../../hooks/sharing";
import { fullTime, shortTime } from "../TranscriptMarker";
import { ItemRow, partyName, scopeText } from "./parts";

const DECIDED: Record<SharingDecisionKind, string> = {
  approved: "Approved",
  denied: "Denied",
  revoked: "Revoked",
  expired: "Expired",
  left: "Left",
};

/** what the history leaves out, in one line, or null when there's nothing to
    say: "Showing the latest 50 of 73. Decisions replaced by a later one aren't
    shown." */
export function historyNote(limits: SharingHistoryLimits | null) {
  if (!limits || (limits.observed === 0 && !limits.conflictsOmitted)) return null;
  const { observed, limit, omitsSuperseded, conflictsOmitted } = limits;
  const left =
    omitsSuperseded && conflictsOmitted
      ? "replaced by a later one, or in conflict,"
      : omitsSuperseded
        ? "replaced by a later one"
        : conflictsOmitted
          ? "in conflict"
          : null;
  const parts = [
    observed > limit && `Showing the latest ${limit} of ${observed}.`,
    left && `Decisions ${left} aren't shown.`,
  ].filter(Boolean);
  return parts.length > 0 ? parts.join(" ") : null;
}

/** One line of the node's sharing history: what happened to whom, for
    what, who decided it and when. */
export function DecisionRow({ decision }: { decision: SharingDecision }) {
  const thisApp = useThisAppDid() === decision.party.did;
  const who = `${partyName(decision.party)}${thisApp ? " (this app)" : ""}`;
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
