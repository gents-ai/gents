import { useEffect, useRef, useState } from "react";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@gents/ui/components/alert-dialog";
import { nodeOf } from "../../hooks/fleetStore";
import { sessionDestination } from "../../hooks/selectionActions";
import { bindNavigationGuard, type Route } from "../lib/router";
import { useApp } from "./AppContext";

type Review = {
  from: string;
  to: string;
  fromName: string;
  toName: string;
  intent: number;
  clientDid: string | undefined;
  proceed: () => void;
  cancel?: () => void;
};

export function NodeNavigationGuard() {
  const app = useApp();
  const [review, setReview] = useState<Review | null>(null);
  const [open, setOpen] = useState(false);
  const pending = useRef<Review | null>(null);
  const approved = useRef<Review | null>(null);
  useEffect(
    () =>
      bindNavigationGuard((route: Route, proceed, cancel) => {
        const selection = app.stores.selection.getState();
        const fleet = app.stores.fleet.getState();
        const to =
          "nodeDid" in route && route.nodeDid
            ? route.nodeDid
            : route.name === "session" && route.sessionId
              ? sessionDestination(fleet, selection, route.sessionId)
              : selection.nodeDid;
        if (!selection.nodeDid || !to || to === selection.nodeDid) return true;
        const key = app.project().draftKey;
        const drafts = app.drafts.getState();
        if (!drafts.drafts[key]?.trim() && !drafts.attachments[key]?.length)
          return true;
        const clientDid = app.stores.client.getState().snapshot?.client?.clientDid;
        const confirmed = approved.current;
        if (
          confirmed &&
          confirmed.from === selection.nodeDid &&
          confirmed.to === to &&
          confirmed.clientDid === clientDid &&
          app.actions.acceptsComposeIntent(confirmed.intent)
        ) {
          approved.current = null;
          return true;
        }
        const fromNode = nodeOf(fleet, selection.nodeDid);
        const toNode = nodeOf(fleet, to);
        const request: Review = {
          from: selection.nodeDid,
          to,
          fromName: fromNode?.node.displayName ?? fromNode?.label ?? selection.nodeDid,
          toName: toNode?.node.displayName ?? toNode?.label ?? to,
          intent: app.actions.captureComposeIntent(),
          clientDid,
          proceed,
          cancel,
        };
        pending.current = request;
        setReview(request);
        setOpen(true);
        return false;
      }),
    [app],
  );

  const dismiss = () => {
    const request = pending.current;
    pending.current = null;
    setOpen(false);
    request?.cancel?.();
  };
  const accept = (request: Review) => {
    if (pending.current !== request) return;
    pending.current = null;
    setOpen(false);
    if (
      app.stores.selection.getState().nodeDid !== request.from ||
      app.stores.client.getState().snapshot?.client?.clientDid !== request.clientDid ||
      !app.actions.acceptsComposeIntent(request.intent)
    )
      return;
    approved.current = request;
    request.proceed();
  };
  return (
    <AlertDialog
      open={open}
      onOpenChange={(open) => {
        if (!open) dismiss();
      }}
    >
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Switch active node?</AlertDialogTitle>
          <AlertDialogDescription>
            Your draft stays with {review?.fromName}. New messages and configuration
            will use {review?.toName}.
          </AlertDialogDescription>
        </AlertDialogHeader>
        {review && (
          <p className="break-all font-mono text-xs text-muted-foreground">
            {review.to}
          </p>
        )}
        <AlertDialogFooter>
          <AlertDialogCancel onClick={dismiss}>Keep current node</AlertDialogCancel>
          <AlertDialogAction
            onClick={() => {
              if (review) accept(review);
            }}
          >
            Switch node
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
