import { useEffect, type ReactNode } from "react";
import type { SendStatus } from "@source-inc/gents-desktop-chat";
import { selectedNodeReadinessDecision } from "@source-inc/gents-desktop-client";
import { Button, buttonVariants } from "@gents/ui/components/button";
import { Spinner } from "@gents/ui/components/spinner";
import { useApp } from "@/app/AppContext";
import type { NodeView } from "../../../hooks/fleetStore";
import { useHomeDid, useNodeConfigReadOnly } from "@/hooks/useClient";
import { isLocalNode } from "@/lib/firstRun";
import { href } from "@/lib/router";
import { useLocalServerRun } from "../agent/LocalServer";

/** Why nothing typed here could be sent, for as long as that holds: the
    node has no inference it can use, the node is offline, it refuses this
    app's credentials, this app has no endpoint for it, or the session takes
    no messages from this app. States
    that clear by themselves (sending, syncing, waiting to see a request) are
    none of these. */
export type ComposerReason =
  | { kind: "inference"; nodeDid: string; name: string; title: string }
  | { kind: "offline"; name: string }
  | { kind: "refused"; name: string }
  | { kind: "noEndpoint"; name: string }
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
  agentDid: string | null,
  status: SendStatus,
): ComposerReason | null {
  if (status.kind !== "disabled") return null;
  if (status.reason === "sessionNotOwned")
    return { kind: "unsendable", text: status.hint };
  if (status.reason !== "runtimeNotConnected" && status.reason !== "agentUnavailable")
    return null;
  if (!deployment) return null;
  const name = deployment.node.displayName ?? deployment.label;
  if (deployment.runtimeConnection === "disconnected") return { kind: "offline", name };
  if (deployment.runtimeConnection === "refused") return { kind: "refused", name };
  if (deployment.runtimeConnection === "noEndpoint")
    return { kind: "noEndpoint", name };
  if (status.reason !== "agentUnavailable") return null;
  const decision = selectedNodeReadinessDecision(deployment, agentDid);
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

/** This computer's own node, stopped, which the person can start from here;
    null for any other node, or before its state is read. */
function useStoppedLocalNode(deployment: NodeView | null) {
  const { actions } = useApp();
  const homeDid = useHomeDid();
  const local = useLocalServerRun();
  const watched =
    deployment !== null && isLocalNode(deployment, homeDid) && local.offered;
  useEffect(
    () => (watched ? actions.watchLocalServer() : undefined),
    [actions, watched],
  );
  return watched && local.known && !local.running ? local : null;
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
  const stopped = useStoppedLocalNode(reason.kind === "offline" ? deployment : null);
  const title =
    reason.kind === "inference"
      ? reason.title
      : reason.kind === "offline"
        ? `${reason.name} is offline`
        : reason.kind === "refused"
          ? `${reason.name} doesn't accept this app`
          : reason.kind === "noEndpoint"
            ? `${reason.name} has no address this app can reach`
            : reason.text;
  const detail =
    reason.kind === "inference"
      ? "Sessions here need a model to answer."
      : reason.kind === "offline"
        ? stopped
          ? "It isn't running on this computer. Start it to send messages here, or send from another node."
          : "Messages can be sent once it's back, or from another node."
        : reason.kind === "refused"
          ? "Ask its owner to restore this device's access; it reconnects by itself once they do. Or send from another node."
          : reason.kind === "noEndpoint"
            ? "Messages can be sent once it has a runtime endpoint, or from another node."
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
              {stopped && (
                <Button
                  size="sm"
                  variant="raised"
                  disabled={stopped.busy || stopped.starting}
                  onClick={stopped.start}
                >
                  {stopped.busy || stopped.starting ? <Spinner /> : null} Start node
                </Button>
              )}
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
