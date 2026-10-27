import { useShallow } from "zustand/react/shallow";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { nodeOf, type FleetState } from "../../hooks/fleetStore";
import { useFleet } from "../hooks/useFleet";
import { agentName } from "./behavior";
import { AgentInitials } from "./parts";

const SHOWN = 3;

/** The agents of the sessions `select` picks, each once by node and agent,
    in the order its first session started. */
export function useWorkerAgents(
  select: (state: FleetState) => readonly SessionSummary[],
): string[] {
  return useFleet(
    useShallow((state) => {
      const seen = new Map<string, string>();
      for (const w of select(state)) {
        const key = `${w.nodeDid}\u0000${w.agentId ?? ""}`;
        if (!seen.has(key))
          seen.set(key, agentName(w.agentId, nodeOf(state, w.nodeDid)));
      }
      return [...seen.values()];
    }),
  );
}

/** Started sessions' agents, packed close: they read as one group beside
    their count, apart from the agent that ran the session. */
export function WorkerAgents({ names }: { names: readonly string[] }) {
  return (
    <span className="flex items-center">
      {names.slice(0, SHOWN).map((name, i) => (
        <AgentInitials
          key={i}
          name={name}
          className="size-5 text-[9px] ring-2 ring-background not-first:-ml-2"
        />
      ))}
    </span>
  );
}
