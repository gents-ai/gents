import { useEffect, useRef } from "react";
import { toast } from "sonner";
import { useHomeDid } from "@/hooks/useClient";
import { useFleet } from "@/hooks/useFleet";
import { useSharingActions } from "@/hooks/useSharing";
import { SHARING_SECTION } from "@/lib/joinInvite";
import { navigate, type Route } from "@/lib/router";
import { useFlag } from "@/preferences";
import { partyName } from "@/screens/sharing/parts";

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
    if (!on || !homeDid) return;
    let live = true;
    const review: Route = { name: "agent", nodeDid: homeDid, section: SHARING_SECTION };
    const check = () => {
      sharing.read(homeDid).then(
        (state) => {
          if (!live || !state) return;
          const waiting = new Set(state.requests.map((r) => r.requestId));
          for (const id of told.current)
            if (!waiting.has(id)) {
              told.current.delete(id);
              toast.dismiss(`join:${id}`);
            }
          for (const request of state.requests) {
            if (told.current.has(request.requestId)) continue;
            told.current.add(request.requestId);
            if (lookingRef.current) continue;
            toast(
              `${partyName(request.from)} asked to join ${nameRef.current ?? "this node"}`,
              {
                id: `join:${request.requestId}`,
                description: "Approve or deny it in Sharing before it expires.",
                duration: Infinity,
                action: { label: "Review", onClick: () => navigate(review) },
              },
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
  }, [looking]);

  return null;
}
