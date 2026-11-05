import { useEffect, useRef, useState } from "react";
import { Button } from "@gents/ui/components/button";
import { MonitorSmartphone } from "lucide-react";
import { toast } from "sonner";
import { useHomeDid } from "@/hooks/useClient";
import { useFleet } from "@/hooks/useFleet";
import { useSharingActions } from "@/hooks/useSharing";
import { SHARING_SECTION } from "@/lib/joinInvite";
import { navigate, type Route } from "@/lib/router";
import { useFlag } from "@/preferences";
import type { JoinRequest } from "../../hooks/sharing";
import { noteWaiting, reviewJoinRequest } from "@/screens/sharing/JoinRequestDialog";
import { sayDenied } from "@/screens/sharing/decided";
import { inNativeShell, windowLabel } from "../../lib/nativeShell";
import { scopeInSentence, shortDid } from "@/screens/sharing/parts";

const MANY = 3;
const MANY_ID = "join:many";

/** Tells the person when someone asks to join this desktop's own node,
    wherever they are in the app: the Sharing section is out of the way, and
    a request lasts only minutes. Each request is told once, and its notice
    goes when the request is decided or expires. */
export function JoinRequestAlerts({ route }: { route: Route }) {
  const sharing = useSharingActions();
  const homeDid = useHomeDid();
  const on = useFlag("sharing");
  const name = useFleet((s) => {
    const node = homeDid ? s.nodes[homeDid] : undefined;
    return node ? (node.node.displayName ?? node.label) : null;
  });
  const told = useRef(new Set<string>());
  /* the Sharing section shows requests itself */
  const looking =
    route.name === "agent" &&
    route.nodeDid === homeDid &&
    route.section === SHARING_SECTION;
  const lookingRef = useRef(looking);
  lookingRef.current = looking;
  const nameRef = useRef(name);
  nameRef.current = name;

  useEffect(() => {
    /* every window runs the app; only the main one speaks for it */
    if (!on || !homeDid || (inNativeShell() && windowLabel() !== "main")) return;
    let live = true;
    const review: Route = { name: "agent", nodeDid: homeDid, section: SHARING_SECTION };
    const check = () => {
      sharing.read(homeDid).then(
        (state) => {
          if (!live || !state) return;
          noteWaiting(homeDid, state.requests);
          const waiting = new Set(state.requests.map((r) => r.requestId));
          for (const id of told.current)
            if (!waiting.has(id)) {
              told.current.delete(id);
              toast.dismiss(`join:${id}`);
            }
          if (lookingRef.current) {
            for (const r of state.requests) told.current.add(r.requestId);
            return;
          }
          const nodeName = nameRef.current ?? "this node";
          /* past a couple, one notice counts them rather than stacking */
          if (state.requests.length >= MANY) {
            for (const id of told.current) toast.dismiss(`join:${id}`);
            for (const r of state.requests) told.current.add(r.requestId);
            toast(`${state.requests.length} requests to join ${nodeName}`, {
              id: MANY_ID,
              description: "Each lasts only a few minutes.",
              duration: Infinity,
              action: { label: "Review", onClick: () => navigate(review) },
            });
            return;
          }
          toast.dismiss(MANY_ID);
          for (const request of state.requests) {
            if (told.current.has(request.requestId)) continue;
            told.current.add(request.requestId);
            toast.custom(
              (id) => (
                <JoinRequestNotice
                  request={request}
                  nodeName={nodeName}
                  onReview={() => {
                    toast.dismiss(id);
                    /* opened after this click: the click that opens it would
                       otherwise count as a press outside it and close it */
                    setTimeout(() => reviewJoinRequest(homeDid, nodeName, request), 0);
                  }}
                  onDeny={() =>
                    sharing.deny(homeDid, request.requestId).then(sayDenied)
                  }
                />
              ),
              { id: `join:${request.requestId}`, duration: Infinity },
            );
          }
        },
        () => {
          /* the Sharing section says what went wrong; a notice can't help */
        },
      );
    };
    check();
    const stop = sharing.subscribe(homeDid, check);
    return () => {
      live = false;
      stop();
    };
  }, [sharing, homeDid, on]);

  useEffect(() => {
    if (!looking) return;
    for (const id of told.current) toast.dismiss(`join:${id}`);
    toast.dismiss(MANY_ID);
  }, [looking]);

  return null;
}

/* one request, with what's needed to decide whether to look: who, what they
   want, and the two answers */
function JoinRequestNotice({
  request,
  nodeName,
  onReview,
  onDeny,
}: {
  request: JoinRequest;
  nodeName: string;
  onReview: () => void;
  onDeny: () => Promise<void>;
}) {
  const [denying, setDenying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /* until a request carries its device's own name, it's named honestly as
     a device and shown by its DID */
  const who = request.from.name ?? "A device";
  return (
    <div
      role="status"
      aria-label={`${who} asks to join ${nodeName}`}
      className="flex w-[356px] items-center gap-3 rounded-2xl border border-border bg-popover p-3 text-popover-foreground shadow-lg"
    >
      <span
        aria-hidden
        className="grid size-9 shrink-0 place-items-center rounded-full bg-muted text-muted-foreground"
      >
        <MonitorSmartphone className="size-4" />
      </span>
      <div className="grid min-w-0 flex-1 gap-0.5">
        <p className="text-sm leading-snug">
          <span className="font-medium">{who}</span> asks to join {nodeName}
        </p>
        <p className="truncate text-xs text-muted-foreground" title={request.from.did}>
          {error ??
            `${shortDid(request.from.did)} · ${request.inviteId ? "your invite" : "no invite"} · wants ${scopeInSentence(request.scope)}`}
        </p>
      </div>
      <Button
        size="sm"
        variant="outline"
        disabled={denying}
        onClick={() => {
          setDenying(true);
          setError(null);
          onDeny().catch((cause: unknown) => {
            setError(cause instanceof Error ? cause.message : String(cause));
            setDenying(false);
          });
        }}
      >
        {denying ? "Denying…" : "Deny"}
      </Button>
      <Button size="sm" disabled={denying} onClick={onReview}>
        Review
      </Button>
    </div>
  );
}
