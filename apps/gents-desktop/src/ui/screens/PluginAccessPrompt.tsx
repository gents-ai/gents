import { shortPath } from "@/lib/paths";
import { useHomeFolder } from "@/hooks/useHomeFolder";
import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@gents/ui/components/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import { toast } from "sonner";
import { newestWins } from "../../lib/reads";
import type {
  PluginApprovalDecision,
  PluginApprovalRequest,
} from "@source-inc/gents-desktop-client";
import { inNativeShell, listenToWindow } from "../../lib/nativeShell";
import { useApp } from "@/app/AppContext";
import { errorText } from "../../lib/errorText";

const PLUGIN_APPROVALS_CHANGED_EVENT = "desktop://plugin-approvals-changed";

export function PluginAccessPrompt() {
  const { actions } = useApp();
  const home = useHomeFolder();
  const [question, setQuestion] = useState<PluginApprovalRequest | null>(null);
  const answering = useRef(false);
  const active = useRef(false);
  const failed = useRef<string | null>(null);
  const reading = useRef(false);
  // announce is edge-triggered with no replay: a change seen while a read is out must re-read
  const dirty = useRef(false);
  const [asks] = useState(newestWins);

  const refresh = useCallback(async () => {
    if (!active.current) return;
    if (answering.current || reading.current) {
      dirty.current = true;
      return;
    }
    reading.current = true;
    const current = asks.begin();
    try {
      const { requests } = await actions.listPendingPluginApprovals();
      if (!current()) return;
      failed.current = null;
      setQuestion(requests[0] ?? null);
    } catch (error) {
      if (!current()) return;
      const text = errorText(error);
      if (failed.current !== text) toast.error(text);
      failed.current = text;
    } finally {
      reading.current = false;
      if (dirty.current && !answering.current) {
        dirty.current = false;
        void refresh();
      }
    }
  }, [actions]);

  useEffect(() => {
    active.current = false;
    let unlisten: (() => void) | undefined;
    let stopped = false;
    let subscription: Promise<void> | undefined;
    const token = inNativeShell()
      ? Array.from(crypto.getRandomValues(new Uint32Array(4))).join("-")
      : null;
    const unregister = async () => {
      if (token === null) return;
      try {
        await actions.unsubscribePluginApprovals(token);
      } catch (error) {
        toast.error(error instanceof Error ? error.message : String(error));
      }
    };
    if (token === null) {
      active.current = true;
      void refresh();
    } else {
      void listenToWindow(PLUGIN_APPROVALS_CHANGED_EVENT, () => {
        if (!stopped) void refresh();
      }).then(
        async (stop) => {
          if (stopped) {
            stop();
            return;
          }
          unlisten = stop;
          try {
            subscription = actions.subscribePluginApprovals(token);
            await subscription;
            if (stopped) return;
            active.current = true;
            void refresh();
          } catch (error) {
            if (!stopped) {
              toast.error(error instanceof Error ? error.message : String(error));
            }
          }
        },
        (error) => {
          if (!stopped) {
            toast.error(error instanceof Error ? error.message : String(error));
          }
        },
      );
    }
    return () => {
      stopped = true;
      active.current = false;
      dirty.current = false;
      asks.supersede();
      unlisten?.();
      void subscription?.then(unregister, () => {});
    };
  }, [actions, refresh]);

  async function answer(decision: PluginApprovalDecision) {
    if (!question || answering.current) return;
    answering.current = true;
    // A decision makes any read that was pending for this prompt stale.
    asks.supersede();
    let answered = false;
    try {
      await actions.decidePluginApproval(question.id, decision);
      answered = true;
      setQuestion(null);
    } catch (error) {
      toast.error(errorText(error));
    } finally {
      answering.current = false;
      if (answered || dirty.current) {
        dirty.current = false;
        void refresh();
      }
    }
  }

  return (
    <Dialog
      open={question !== null}
      onOpenChange={(open) => !open && void answer("deny")}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{question?.prompt}</DialogTitle>
          <DialogDescription>
            {question?.subject.kind === "filesystem" ? (
              <>
                Always allow this folder remembers{" "}
                <span title={question.subject.folder}>
                  {shortPath(question.subject.folder, { home })}
                </span>
                .
              </>
            ) : question?.subject.kind === "command" ? (
              <>
                Allow once runs only this command in{" "}
                <span title={question.subject.cwd}>
                  {shortPath(question.subject.cwd, { home })}
                </span>
                .
              </>
            ) : null}
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="outline" onClick={() => void answer("deny")}>
            Deny
          </Button>
          {question?.answers.includes("file") && (
            <Button variant="outline" onClick={() => void answer("file")}>
              Always allow this file
            </Button>
          )}
          {question?.answers.includes("always") && (
            <Button variant="outline" onClick={() => void answer("always")}>
              Always allow this folder
            </Button>
          )}
          {question?.answers.includes("invocation") && (
            <Button variant="outline" onClick={() => void answer("invocation")}>
              Remember this exact command
            </Button>
          )}
          <Button onClick={() => void answer("once")}>Allow once</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
