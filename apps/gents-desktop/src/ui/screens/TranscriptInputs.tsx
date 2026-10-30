import type { ReactNode } from "react";
import { Bot, Clock, Play, Timer, Workflow, Zap } from "lucide-react";
import type {
  RenderedTimelineItem,
  RequestLifecycleState,
  RequestOriginView,
} from "@source-inc/gents-desktop-client";
import type { NodeView } from "../../hooks/fleetStore";
import { useSelectedNode } from "@/hooks/useClient";
import { useFleet } from "../hooks/useFleet";

type Reconstruction = Extract<
  RenderedTimelineItem,
  { kind: "automatedInput" }
>["reconstruction"];
import { Badge } from "@gents/ui/components/badge";
import { UserMessage } from "@gents/ui/conversation";
import { cn } from "@gents/ui/lib/utils";
import { href } from "@/lib/router";
import { holdRow } from "@/lib/scroll";
import { Markdown } from "./Markdown";

export function pendingInputState(
  lifecycleState: RequestLifecycleState | null | undefined,
  foldedIntoRequestId?: string | null,
): string | null {
  if (foldedIntoRequestId || !lifecycleState) return null;
  switch (lifecycleState) {
    case "interrupted":
      return "Not sent · interrupted";
    case "failed":
      return "Not sent · failed";
    case "dead":
      return "Not sent · expired";
    case "workspaceBindingPending":
    case "pending":
    case "claimed":
    case "processing":
    case "completed":
    case "superseded":
      return null;
  }
}

function triggerName(triggerId: string, node: NodeView | null) {
  const trigger = node?.triggers?.find((t) => t.config.trigger_id === triggerId);
  const task = node?.tasks?.find((t) => t.taskId === trigger?.config.task_id);
  return trigger?.config.display_name ?? task?.name ?? triggerId;
}

export function originLabel(
  origin: RequestOriginView,
  node: NodeView | null,
  senderTitle: string | null,
): string {
  switch (origin.kind) {
    case "sessionMessage": {
      if (senderTitle) return `Message from ${senderTitle}`;
      if (origin.senderSessionId) return "Message from another session";
      return origin.senderNodeDid === node?.nodeDid
        ? "Message from another session"
        : "Message from another node";
    }
    case "trigger": {
      const name = triggerName(origin.triggerId, node);
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
      return "Background activity";
  }
}

function OriginIcon({ origin }: { origin: RequestOriginView }) {
  const className = "size-4 shrink-0";
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

function unavailableReason(reconstruction: Reconstruction | undefined) {
  switch (reconstruction?.state) {
    case "loading":
      return "Still syncing this input.";
    case "denied":
      return "This input is not shared with this device.";
    case "invalid":
      return "This input could not be read.";
    default:
      return null;
  }
}

export function AutomatedInput({
  origin,
  content,
  reconstruction,
  state = null,
}: {
  origin: RequestOriginView;
  content: string | null | undefined;
  reconstruction?: Reconstruction;
  state?: string | null;
}) {
  const unavailable = unavailableReason(reconstruction);
  const node = useSelectedNode();
  const senderSessionId =
    origin.kind === "sessionMessage" ? origin.senderSessionId : null;
  const senderTitle = useFleet((fleet) =>
    senderSessionId ? (fleet.bySessionId[senderSessionId]?.title ?? null) : null,
  );
  const label = originLabel(origin, node, senderTitle);
  const body =
    origin.kind === "sessionMessage" ? (origin.displayBody ?? content) : content;
  const control =
    origin.kind === "backgroundCompletion" || origin.kind === "goalContinuation";
  return (
    <section
      data-testid="automated-input"
      aria-label={label}
      className="min-w-0 rounded-xl border border-border/60 px-4 py-3"
    >
      <header className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
        <OriginIcon origin={origin} />
        <span className="min-w-0 flex-1 break-words font-medium">{label}</span>
        {senderSessionId && (
          <a
            href={href({ name: "session", sessionId: senderSessionId })}
            className="underline decoration-border underline-offset-4 hover:text-foreground"
          >
            Open sender
          </a>
        )}
        {state && (
          <Badge variant="outline" className="font-normal">
            {state}
          </Badge>
        )}
      </header>
      {unavailable ? (
        <p
          className="mt-2 text-sm text-muted-foreground"
          data-testid="automated-input-unavailable"
        >
          {unavailable}
        </p>
      ) : (
        <>
          {!control && body && (
            <div
              className="prose-app mt-3 break-words"
              data-testid="automated-input-body"
            >
              <Markdown>{body}</Markdown>
            </div>
          )}
          <details
            className="mt-2 min-w-0"
            onClick={(event) => holdRow(event.currentTarget)}
          >
            <summary className="cursor-pointer text-xs text-muted-foreground">
              Model input
            </summary>
            <pre
              className="mt-3 max-h-96 overflow-auto font-mono text-xs leading-relaxed break-words whitespace-pre-wrap text-muted-foreground"
              data-testid="automated-input-content"
            >
              {content ?? ""}
            </pre>
          </details>
        </>
      )}
    </section>
  );
}

export function UserInputWithState({
  content,
  state,
  actions,
}: {
  content: ReactNode;
  state: string | null;
  actions?: Parameters<typeof UserMessage>[0]["actions"];
}) {
  if (!state) return <UserMessage actions={actions}>{content}</UserMessage>;
  return (
    <div
      className={cn(
        "relative mb-2",
        (state === "Queued" || state === "Pending") && "opacity-70",
      )}
    >
      <UserMessage actions={actions}>{content}</UserMessage>
      <Badge
        variant="outline"
        title={state === "Pending" ? "Waiting for the next safe step" : undefined}
        className="absolute right-4 bottom-0 translate-y-1/2 border-border/60 bg-background text-[10px] font-normal text-muted-foreground"
      >
        {state}
      </Badge>
    </div>
  );
}
