/* The pieces both session views put around their composer. */
import type { SendStatus } from "@source-inc/gents-desktop-chat";
import { cn } from "@gents/ui/lib/utils";
import type { ShellView } from "@/../hooks/shellView";

/** Add only local composer emptiness; every other blocker belongs to ClientShell. */
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

/** Display the existing workflow owner's observation, never infer queue health. */
/* The line under the composer holds its place whether or not it has
   anything to say: a status arriving and leaving moved the composer up
   and down, and a person's eye with it. One line of the small size is
   always reserved; an error, being rarer and longer, may still grow it. */
export function SessionSubmissionStatus({
  activityStatus,
  hint,
  reserve = true,
}: {
  activityStatus: ShellView["shellProjection"]["activityStatus"];
  hint?: string | null;
  /** a new chat has no transcript above to hold still, so its line may
      take no room until it has something to say */
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
