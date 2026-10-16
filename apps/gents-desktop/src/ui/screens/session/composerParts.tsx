import type { SendStatus } from "@source-inc/gents-desktop-chat";
import { cn } from "@gents/ui/lib/utils";
import type { ShellView } from "@/../hooks/shellView";

export function presentedComposerSendStatus(
  draft: string,
  canonicalNonEmptyStatus: SendStatus,
): SendStatus {
  return draft.trim()
    ? canonicalNonEmptyStatus
    : {
        kind: "disabled",
        reason: "composerEmpty",
        hint: "Type a message to send",
      };
}

export function SessionSubmissionStatus({
  activityStatus,
  hint,
  reserve = true,
}: {
  activityStatus: ShellView["shellProjection"]["activityStatus"];
  hint?: string | null;
  reserve?: boolean;
}) {
  const status = activityStatus;
  const quiet = !status && hint;
  const empty = !status && !quiet;
  return (
    <div
      className={cn("px-1", reserve ? "mt-2 min-h-4" : !empty && "mt-2")}
      data-testid="composer-status"
    >
      {status && (
        <div
          role="status"
          title={status.detail}
          className="text-xs leading-4 text-muted-foreground animate-in fade-in-0 duration-200 motion-reduce:animate-none"
        >
          <span>{status.label}</span>
        </div>
      )}
      {quiet && <p className="text-xs leading-4 text-muted-foreground">{hint}</p>}
    </div>
  );
}
