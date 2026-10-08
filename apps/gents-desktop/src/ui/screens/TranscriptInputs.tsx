/* Input the model receives that a person did not type, and messages a person
   typed that are still waiting behind the running turn. Every fact here comes
   from the bridge: the request's origin, its lifecycle state, and the
   session's queue; nothing is inferred from text. */
import { useState } from "react";
import { Bot, ChevronDown, Clock, Play, Timer, Workflow, Zap } from "lucide-react";
import type {
  DeploymentView,
  PendingTurnView,
  RequestOriginView,
} from "@source-inc/gents-desktop-client";
import { Badge } from "@gents/ui/components/badge";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import { UserMessage } from "@gents/ui/conversation";
import { cn } from "@gents/ui/lib/utils";
import { href } from "@/lib/router";

/* the state a person-typed input that has not been answered is in, worded
   for the person: nothing for one being answered now */
export function pendingInputState(
  lifecycleState: string | null | undefined,
  foldedIntoRequestId?: string | null,
): string | null {
  if (foldedIntoRequestId) return null;
  switch (lifecycleState) {
    case "pending":
    case "workspaceBindingPending":
      return "Queued";
    case "interrupted":
      return "Not sent · interrupted";
    case "failed":
      return "Not sent · failed";
    case "dead":
      return "Not sent · expired";
    default:
      return null;
  }
}

function triggerName(triggerId: string, deployment: DeploymentView | null) {
  const trigger = deployment?.triggers?.find((t) => t.config.trigger_id === triggerId);
  const task = deployment?.tasks?.find((t) => t.taskId === trigger?.config.task_id);
  return trigger?.config.display_name ?? task?.name ?? triggerId;
}

/* who or what sent it, in a few words */
export function originLabel(
  origin: RequestOriginView,
  deployment: DeploymentView | null,
): string {
  switch (origin.kind) {
    case "sessionMessage": {
      const session = deployment?.sessions?.find(
        (s) => s.sessionId === origin.senderSessionId,
      );
      if (session?.title) return `Message from ${session.title}`;
      if (origin.senderSessionId) return "Message from another session";
      return origin.senderAgentDid === deployment?.agentDid
        ? "Message from another session"
        : "Message from another agent";
    }
    case "trigger": {
      const name = triggerName(origin.triggerId, deployment);
      return origin.triggerKind === "schedule"
        ? `Schedule · ${name}`
        : origin.triggerKind === "event"
          ? `Event · ${name}`
          : `Task run · ${name}`;
    }
    case "goalContinuation":
      return origin.sequence != null
        ? `Goal continuation · ${origin.sequence}`
        : "Goal continuation";
    case "backgroundCompletion":
      return "Background work finished";
  }
}

function OriginIcon({ origin }: { origin: RequestOriginView }) {
  const className = "size-3.5 shrink-0";
  switch (origin.kind) {
    case "sessionMessage":
      return <Bot className={className} />;
    case "trigger":
      return origin.triggerKind === "schedule" ? (
        <Timer className={className} />
      ) : origin.triggerKind === "event" ? (
        <Zap className={className} />
      ) : (
        <Play className={className} />
      );
    case "goalContinuation":
      return <Workflow className={className} />;
    case "backgroundCompletion":
      return <Clock className={className} />;
  }
}

function firstLine(content: string | null | undefined) {
  return (
    content
      ?.split("\n")
      .map((line) => line.trim())
      .find(Boolean) ?? ""
  );
}

/* A compact row that opens to exactly what the model received. */
export function AutomatedInput({
  origin,
  content,
  state = null,
  deployment,
}: {
  origin: RequestOriginView;
  content: string | null | undefined;
  /* the request's state while it has not run, from its lifecycle */
  state?: string | null;
  deployment: DeploymentView | null;
}) {
  const [open, setOpen] = useState(false);
  const label = originLabel(origin, deployment);
  const preview = firstLine(content);
  return (
    <Collapsible
      open={open}
      onOpenChange={setOpen}
      className="text-xs text-muted-foreground"
      data-testid="automated-input"
    >
      <div className="flex min-w-0 items-center gap-1.5">
        <CollapsibleTrigger
          className="-mx-2 -my-1 flex min-w-0 flex-1 cursor-pointer items-center gap-1.5 rounded-lg px-2 py-1 text-left transition-colors hover:bg-accent/30 hover:text-foreground"
          aria-label={`${label}: ${open ? "hide" : "show"} what the agent received`}
        >
          <OriginIcon origin={origin} />
          <span className="shrink-0 font-medium text-foreground/80">{label}</span>
          {preview && (
            <span className="min-w-0 truncate" data-testid="automated-input-preview">
              {preview}
            </span>
          )}
          <ChevronDown
            className={cn(
              "ml-auto size-3.5 shrink-0 transition-transform",
              open && "rotate-180",
            )}
          />
        </CollapsibleTrigger>
        {origin.kind === "sessionMessage" && origin.senderSessionId && (
          <a
            href={href({ name: "session", sessionId: origin.senderSessionId })}
            className="shrink-0 underline decoration-border underline-offset-4 hover:text-foreground"
          >
            Open
          </a>
        )}
        {state && (
          <Badge
            variant="outline"
            className="shrink-0 border-border/60 text-[10px] font-normal text-muted-foreground"
          >
            {state}
          </Badge>
        )}
      </div>
      <CollapsibleContent className="mt-1 border-l border-border pl-3">
        <pre
          className="max-h-96 overflow-auto font-mono text-[11px] leading-relaxed whitespace-pre-wrap text-muted-foreground"
          data-testid="automated-input-content"
        >
          {content ?? ""}
        </pre>
      </CollapsibleContent>
    </Collapsible>
  );
}

/* A person's message with the state it is in, seated on the bubble's bottom
   edge the way a sender's queued chip is. */
export function UserInputWithState({
  content,
  state,
  actions,
}: {
  content: string;
  state: string | null;
  actions?: Parameters<typeof UserMessage>[0]["actions"];
}) {
  if (!state) return <UserMessage actions={actions}>{content}</UserMessage>;
  return (
    <div className={cn("relative mb-2", state === "Queued" && "opacity-70")}>
      <UserMessage actions={actions}>{content}</UserMessage>
      <Badge
        variant="outline"
        className="absolute right-4 bottom-0 translate-y-1/2 border-border/60 bg-background text-[10px] font-normal text-muted-foreground"
      >
        {state}
      </Badge>
    </div>
  );
}

/* Messages waiting behind the running turn, in the order they will be read.
   They have not entered the transcript; once a turn claims them they appear
   there as its input and leave this list. */
export function QueuedInputs({
  queued,
  deployment,
}: {
  queued: PendingTurnView[];
  deployment: DeploymentView | null;
}) {
  if (queued.length === 0) return null;
  return (
    <div
      className="grid gap-3"
      data-testid="queued-inputs"
      aria-label="Queued messages"
    >
      {queued.map((turn) => (
        <div key={turn.requestId} data-testid="queued-input">
          {turn.origin ? (
            <AutomatedInput
              origin={turn.origin}
              content={turn.content}
              state="Queued"
              deployment={deployment}
            />
          ) : (
            <UserInputWithState content={turn.content} state="Queued" />
          )}
        </div>
      ))}
    </div>
  );
}
