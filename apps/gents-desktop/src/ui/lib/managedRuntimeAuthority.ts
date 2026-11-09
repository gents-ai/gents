import type { ManagedServerAuthorityInput } from "@source-inc/gents-desktop-client";

export function authorityForSelection(
  toolCeiling: ManagedServerAuthorityInput["toolCeiling"],
  toolRoot: string | null,
): ManagedServerAuthorityInput | null {
  if (toolCeiling === "meta-only") return { toolCeiling, toolRoot: null };
  return toolRoot ? { toolCeiling, toolRoot } : null;
}

export function authoritySummary(authority: ManagedServerAuthorityInput): string {
  switch (authority.toolCeiling) {
    case "readwrite":
      return "The managed runtime can read and modify files under this root and run unrestricted commands as your user. Commands are not a filesystem sandbox and may reach other locations your OS account can access; operating-system privacy controls still apply.";
    case "readonly":
      return "The managed runtime can read files under this root and run its restricted read-only command set. It cannot modify files through host tools.";
    case "meta-only":
      return "The managed runtime receives no host file or shell authority. Configuration and remote services remain available where separately configured.";
  }
}

export function authoritiesEqual(
  left: ManagedServerAuthorityInput,
  right: ManagedServerAuthorityInput,
): boolean {
  return left.toolCeiling === right.toolCeiling && left.toolRoot === right.toolRoot;
}

export type AuthorityRestartObservation = {
  rootChanged: boolean;
  nodeObserved: boolean;
  agentPresent: boolean;
};
export type AuthorityRestartRefusal = "nodeUnobserved" | "noAgent";
export type AuthorityRestartAction =
  "keepCurrentFolder" | "retryWhenLoaded" | "addAgent";
export type AuthorityRestartPlan =
  | { kind: "restart" }
  | { kind: "publishThenRestart" }
  | {
      kind: "refused";
      cause: AuthorityRestartRefusal;
      nextActions: AuthorityRestartAction[];
    };

/** Lean owner: `ClientShell.AuthorityRestart.plan` and `nextActions`. The
    restarted runtime admits only a published root, so a changed root is
    published first, and a change that cannot be published is refused before
    any write. */
export function authorityRestartPlan({
  rootChanged,
  nodeObserved,
  agentPresent,
}: AuthorityRestartObservation): AuthorityRestartPlan {
  if (!rootChanged) return { kind: "restart" };
  if (!nodeObserved)
    return {
      kind: "refused",
      cause: "nodeUnobserved",
      nextActions: ["keepCurrentFolder", "retryWhenLoaded"],
    };
  if (!agentPresent)
    return {
      kind: "refused",
      cause: "noAgent",
      nextActions: ["keepCurrentFolder", "addAgent"],
    };
  return { kind: "publishThenRestart" };
}

const REFUSAL_CAUSE: Record<AuthorityRestartRefusal, string> = {
  nodeUnobserved:
    "The new folder must be published for an agent before the restart, and this node's agents have not loaded yet.",
  noAgent:
    "The new folder must be published for an agent before the restart, and this node has no agent.",
};
const NEXT_ACTION: Record<
  Exclude<AuthorityRestartAction, "keepCurrentFolder">,
  string
> = {
  retryWhenLoaded: "wait for the node's agents to appear and restart again",
  addAgent: "add an agent to this node and restart again",
};

/** The refusal's cause followed by its next actions, as shown to the User. */
export function authorityRefusalText(
  plan: Extract<AuthorityRestartPlan, { kind: "refused" }>,
): string {
  const actions = plan.nextActions.map((action) =>
    action === "keepCurrentFolder" ? "keep the current folder" : NEXT_ACTION[action],
  );
  return `${REFUSAL_CAUSE[plan.cause]} To continue, ${actions.join(", or ")}.`;
}
