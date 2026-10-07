/* A running plugin call asking whether it may read a path outside the
   working folder and the allowed folders. The call waits for the answer. */
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
import { call, message } from "./agent/bridgeCall";

type Decision = "once" | "file" | "always" | "deny";

interface Question {
  id: string;
  prompt: string;
  folder: string;
  isDir: boolean;
}

const POLL_MS = 1000;

export function PluginAccessPrompt() {
  const [question, setQuestion] = useState<Question | null>(null);
  const answering = useRef(false);
  const failed = useRef<string | null>(null);
  /* one poll out at a time; an answer outdates the one out, which may have
     read the queue before the answer reached the host */
  const polling = useRef(false);
  const asked = useRef(0);

  const refresh = useCallback(async () => {
    if (answering.current || polling.current) return;
    polling.current = true;
    const ask = ++asked.current;
    try {
      const { requests } = await call<{ requests: Question[] }>(
        "desktop_plugin_approvals_pending",
      );
      if (ask !== asked.current) return;
      failed.current = null;
      setQuestion(requests[0] ?? null);
    } catch (error) {
      if (ask !== asked.current) return;
      const text = message(error);
      if (failed.current !== text) toast.error(text);
      failed.current = text;
    } finally {
      polling.current = false;
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(timer);
  }, [refresh]);

  async function answer(decision: Decision) {
    if (!question) return;
    answering.current = true;
    asked.current += 1;
    try {
      await call("desktop_plugin_approval_decide", { id: question.id, decision });
      setQuestion(null);
    } catch (error) {
      toast.error(message(error));
    } finally {
      answering.current = false;
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
            Always allow this folder remembers {question?.folder}.
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="outline" onClick={() => void answer("deny")}>
            Deny
          </Button>
          {question && !question.isDir && (
            <Button variant="outline" onClick={() => void answer("file")}>
              Always allow this file
            </Button>
          )}
          <Button variant="outline" onClick={() => void answer("always")}>
            Always allow this folder
          </Button>
          <Button onClick={() => void answer("once")}>Allow once</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
