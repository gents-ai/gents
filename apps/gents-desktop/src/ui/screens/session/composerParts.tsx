/* The pieces both session views put around their composer. */
import type { ReactNode } from "react";
import type { SendStatus } from "@source-inc/gents-desktop-chat";
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

/* The line under the composer: the chat's folder at its start, the send's
   status at its end. It keeps one line of room whether or not either has
   anything to show, so a status arriving and leaving never moves the
   composer, or a person's eye with it. */
export function ComposerFooter({
  folder,
  status,
}: {
  folder: ReactNode;
  status: ReactNode;
}) {
  return (
    <div className="mt-1.5 flex min-h-7 items-center gap-3 px-1">
      {folder}
      <div className="ml-auto min-w-0 text-right">{status}</div>
    </div>
  );
}

/** Display the existing workflow owner's observation, never infer queue health. */
/* A running turn is left out: the transcript's activity line and the Stop
   button already say it, and every message sent during it steers. */
export function SessionSubmissionStatus({
  activityStatus,
  hint,
}: {
  activityStatus: ShellView["shellProjection"]["activityStatus"];
  hint?: string | null;
}) {
  const status = activityStatus?.kind === "working" ? null : activityStatus;
  const quiet = !status && hint;
  return (
    <div className="min-w-0" data-testid="composer-status">
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
