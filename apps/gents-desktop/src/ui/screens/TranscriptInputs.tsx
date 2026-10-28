import type { ReactNode } from "react";
import { Bot, Clock, Play, Timer, Workflow, Zap } from "lucide-react";
import type {
  RenderedTimelineItem,
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
import { ToolStep, UserMessage } from "@gents/ui/conversation";
import { cn } from "@gents/ui/lib/utils";
import { href } from "@/lib/router";

export function pendingInputState(
  lifecycleState: string | null | undefined,
  foldedIntoRequestId?: string | null,
): string | null {
  if (foldedIntoRequestId) return null;
  switch (lifecycleState) {
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
      return "Background work finished";
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

function firstLine(content: string | null | undefined) {
  return (
    content
      ?.split("\n")
      .map((line) => line.trim())
      .find(Boolean) ?? ""
  );
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
  const preview = firstLine(content);
  return (
    <div data-testid="automated-input">
      <ToolStep
        label={preview ? `${label} · ${preview}` : label}
        copyText={content ?? label}
        icon={<OriginIcon origin={origin} />}
        meta={
          senderSessionId || state ? (
            <span className="flex items-center gap-2 font-sans">
              {senderSessionId && (
                <a
                  href={href({ name: "session", sessionId: senderSessionId })}
                  className="underline decoration-border underline-offset-4 hover:text-foreground"
                >
                  Open
                </a>
              )}
              {state && (
                <Badge
                  variant="outline"
                  className="border-border/60 text-[10px] font-normal text-muted-foreground"
                >
                  {state}
                </Badge>
              )}
            </span>
          ) : undefined
        }
      >
        {unavailable ? (
          <p
            className="text-xs text-muted-foreground"
            data-testid="automated-input-unavailable"
          >
            {unavailable}
          </p>
        ) : (
          <pre
            className="max-h-96 overflow-auto font-mono text-[11px] leading-relaxed whitespace-pre-wrap text-muted-foreground"
            data-testid="automated-input-content"
          >
            {content ?? ""}
          </pre>
        )}
      </ToolStep>
    </div>
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
