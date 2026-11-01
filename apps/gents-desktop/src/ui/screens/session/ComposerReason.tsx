import type { ReactNode } from "react";
import type { SendStatus } from "@source-inc/gents-desktop-chat";
import { selectedNodeReadinessDecision } from "@source-inc/gents-desktop-client";
import { buttonVariants } from "@gents/ui/components/button";
import type { NodeView } from "../../../hooks/fleetStore";
import { useNodeConfigReadOnly } from "@/hooks/useClient";
import { href } from "@/lib/router";

/** Why nothing typed here could be sent, for as long as that holds: the
    node has no inference it can use, the node is offline, or the session
    takes no messages from this app. States that clear by themselves
    (sending, syncing, waiting to see a request) are none of these. */
export type ComposerReason =
  | { kind: "inference"; nodeDid: string; name: string; title: string }
  | { kind: "offline"; name: string }
  | { kind: "unsendable"; text: string };

/* a backend that is down for now comes back by itself, so it isn't here */
const INFERENCE_TITLE: Record<string, (name: string) => string> = {
  backend_not_configured: (name) => `${name} has no inference set up`,
  backend_disabled: (name) => `${name}'s inference is turned off`,
  inference_profile_invalid: (name) => `${name}'s model settings need fixing`,
  credentials_required: (name) => `${name}'s inference account needs you to sign in`,
  subscription_not_entitled: (name) =>
    `${name}'s inference account can't be used for this`,
};

export function composerReason(
  deployment: NodeView | null,
  agentId: string | null,
  status: SendStatus,
): ComposerReason | null {
  if (status.kind !== "disabled") return null;
  if (status.reason === "sessionNotOwned")
    return { kind: "unsendable", text: status.hint };
  if (status.reason !== "routeNotReady" && status.reason !== "agentUnavailable")
    return null;
  if (!deployment) return null;
  const name = deployment.node.displayName ?? deployment.label;
  if (!deployment.dialSucceeded) return { kind: "offline", name };
  if (status.reason !== "agentUnavailable") return null;
  const decision = selectedNodeReadinessDecision(deployment, agentId);
  if (decision.kind !== "unavailable") return null;
  const title = INFERENCE_TITLE[decision.reason];
  return title
    ? { kind: "inference", nodeDid: deployment.nodeDid, name, title: title(name) }
    : null;
}

const SWITCH = (
  <a
    href={href({ name: "nodes" })}
    className={buttonVariants({ size: "sm", variant: "outline" })}
  >
    Switch node
  </a>
);

function SetUp({ deployment }: { deployment: NodeView }) {
  const readOnly = useNodeConfigReadOnly(deployment);
  /* a node whose settings can't be changed from here is set up on its host */
  return readOnly ? (
    <span className="text-xs text-muted-foreground">Set it up on that machine</span>
  ) : (
    <a
      href={href({ name: "agent", nodeDid: deployment.nodeDid, section: "inference" })}
      className={buttonVariants({ size: "sm", variant: "raised" })}
    >
      Set up inference
    </a>
  );
}

/** The box that stands in the composer's place while `reason` holds; the
    composer keeps its cell beneath, so the page doesn't move. */
export function ComposerReasonBox({
  reason,
  deployment,
  leading,
}: {
  reason: ComposerReason;
  deployment: NodeView | null;
  /** what still applies, such as the agent picker, at the box's foot */
  leading?: ReactNode;
}) {
  const title =
    reason.kind === "inference"
      ? reason.title
      : reason.kind === "offline"
        ? `${reason.name} is offline`
        : reason.text;
  const detail =
    reason.kind === "inference"
      ? "Sessions here need a model to answer."
      : reason.kind === "offline"
        ? "Messages can be sent once it's back, or from another node."
        : null;
  return (
    <div className="[grid-area:1/1]" data-testid="composer-reason">
      <div
        role="status"
        className="flex h-full flex-col justify-between gap-3 rounded-2xl border border-border/60 bg-raised px-4 py-3"
      >
        <div className="min-w-0">
          <p className="text-sm font-medium">{title}</p>
          {detail && <p className="text-xs text-muted-foreground">{detail}</p>}
        </div>
        {(leading || reason.kind !== "unsendable") && (
          <div className="flex flex-wrap items-center gap-2">
            {leading}
            <span className="ml-auto flex flex-wrap items-center gap-2">
              {reason.kind !== "unsendable" && SWITCH}
              {reason.kind === "inference" && deployment && (
                <SetUp deployment={deployment} />
              )}
            </span>
          </div>
        )}
      </div>
    </div>
  );
}
