import { useMemo } from "react";
import { useShallow } from "zustand/react/shallow";
import type { AgentView, SessionSummary } from "@source-inc/gents-desktop-client";
import { nodeOf, type FleetState } from "../../hooks/fleetStore";
import { useFleet } from "../hooks/useFleet";
import { agentOf } from "@/lib/agents";
import { agentName, isShared, sharedNames } from "./behavior";
import { AgentInitials } from "./parts";

const SHOWN = 3;

export type WorkerAgent = { agentDid: string | null; displayName: string };

/** The agents of the sessions `select` picks, each once by node and agent,
    in the order its first session started. */
export function useWorkerAgents(
  select: (state: FleetState) => readonly SessionSummary[],
): WorkerAgent[] {
  /* the listed agent itself, so the selection holds while nothing changes;
     an agent its node doesn't list stays its DID */
  const found = useFleet(
    useShallow((state) => {
      const seen = new Map<string, AgentView | string | null>();
      for (const w of select(state)) {
        const key = `${w.nodeDid}\u0000${w.agentDid ?? ""}`;
        if (!seen.has(key))
          seen.set(key, agentOf(nodeOf(state, w.nodeDid), w.agentDid) ?? w.agentDid);
      }
      return [...seen.values()];
    }),
  );
  return useMemo(
    () =>
      found.map((a) =>
        a !== null && typeof a === "object"
          ? { agentDid: a.agentDid, displayName: a.displayName }
          : { agentDid: a, displayName: agentName(a, null) },
      ),
    [found],
  );
}

/** Started sessions' agents, packed close: they read as one group beside
    their count, apart from the agent that ran the session. */
export function WorkerAgents({ agents }: { agents: readonly WorkerAgent[] }) {
  const shown = agents.slice(0, SHOWN);
  const shared = sharedNames(shown);
  return (
    <span className="flex items-center">
      {shown.map((a, i) => (
        <AgentInitials
          key={i}
          name={a.displayName}
          agentDid={a.agentDid}
          shared={isShared(shared, a.displayName)}
          className="size-5 text-[9px] ring-2 ring-background not-first:-ml-2"
        />
      ))}
    </span>
  );
}
