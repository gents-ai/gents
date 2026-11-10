import { useState } from "react";
import { MonitorSmartphone } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import type { JoinRequest } from "../../../hooks/sharing";
import { useHomeDid } from "@/hooks/useClient";
import { useFleet } from "@/hooks/useFleet";
import { useSharing, useSharingActions } from "@/hooks/useSharing";
import { useFlag } from "@/preferences";
import { span, useMinute } from "../time";
import { sayDenied } from "./decided";
import { reviewJoinRequest } from "./JoinRequestDialog";
import { errorMessage, scopeInSentence, shortDid } from "./parts";

/** the requests waiting to join this desktop's own node, or null when
    there are none or sharing is off */
export function useMailboxRequests(): {
  nodeDid: string;
  name: string;
  requests: JoinRequest[];
} | null {
  const on = useFlag("sharing");
  const homeDid = useHomeDid();
  const view = useSharing(on ? homeDid : null);
  const name = useFleet((s) => {
    const node = homeDid ? s.nodes[homeDid] : undefined;
    return node ? (node.node.displayName ?? node.label) : null;
  });
  if (!on || !homeDid || view.status !== "ready") return null;
  if (view.state.requests.length === 0) return null;
  return { nodeDid: homeDid, name: name ?? "this node", requests: view.state.requests };
}

/** The requests waiting to join this desktop's own node, at the top of the
    Mailbox, each with Deny and Review: a request lasts only minutes, so it
    waits with the other things that need the person rather than only in
    Sharing. */
export function MailboxRequests({
  nodeDid,
  name,
  requests,
}: {
  nodeDid: string;
  name: string;
  requests: JoinRequest[];
}) {
  return (
    <ul aria-label="Requests to join" className="mt-5" data-testid="mailbox-requests">
      {requests.map((request) => (
        <RequestItem
          key={request.requestId}
          nodeDid={nodeDid}
          nodeName={name}
          request={request}
        />
      ))}
    </ul>
  );
}

function RequestItem({
  nodeDid,
  nodeName,
  request,
}: {
  nodeDid: string;
  nodeName: string;
  request: JoinRequest;
}) {
  const sharing = useSharingActions();
  const now = useMinute();
  const [denying, setDenying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /* until a request carries its device's own name, it's named honestly as a
     device and shown by its DID */
  const who = request.from.name ?? "A device";
  const left = Date.parse(request.expiresAt) - now;
  return (
    <li
      aria-label={`${who} asks to join ${nodeName}`}
      className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-3 py-2.5"
    >
      <span
        aria-hidden
        className="grid size-8 shrink-0 place-items-center rounded-full bg-muted text-muted-foreground"
      >
        <MonitorSmartphone className="size-4" />
      </span>
      <span className="min-w-0">
        <span className="block truncate text-sm text-foreground">
          <span className="font-medium">{who}</span> asks to join {nodeName}
        </span>
        <span
          className="mt-0.5 block truncate text-xs text-muted-foreground"
          title={request.from.did}
        >
          {error ??
            [
              "Join request",
              `wants ${scopeInSentence(request.scope)}`,
              shortDid(request.from.did),
              left > 0 ? `${span(left)} left` : "ending",
            ].join(" · ")}
        </span>
      </span>
      <span className="flex shrink-0 gap-2">
        <Button
          size="sm"
          variant="outline"
          disabled={denying}
          onClick={() => {
            setDenying(true);
            setError(null);
            sharing
              .deny(nodeDid, request.requestId)
              .then(sayDenied)
              .catch((cause: unknown) => {
                setError(errorMessage(cause));
                setDenying(false);
              });
          }}
        >
          {denying ? "Denying…" : "Deny"}
        </Button>
        <Button
          size="sm"
          disabled={denying}
          onClick={() => reviewJoinRequest(nodeDid, nodeName, request)}
        >
          Review
        </Button>
      </span>
    </li>
  );
}
