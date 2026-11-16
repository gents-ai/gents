import { useEffect, useState, type ReactNode } from "react";
import type { SendStatus } from "@source-inc/gents-desktop-chat";
import type { ShellView } from "@/../hooks/shellView";

export function presentedComposerSendStatus(
  draft: string,
  canonicalNonEmptyStatus: SendStatus,
  hasAttachments = false,
): SendStatus {
  return draft.trim() || hasAttachments
    ? canonicalNonEmptyStatus
    : {
        kind: "disabled",
        reason: "composerEmpty",
        hint: "Type a message to send",
      };
}

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

const REFRESH_LINE_DELAY_MS = 400;

function useShownAfter(on: boolean, delayMs: number) {
  const [shown, setShown] = useState(false);
  useEffect(() => {
    if (!on) {
      setShown(false);
      return;
    }
    const timer = setTimeout(() => setShown(true), delayMs);
    return () => clearTimeout(timer);
  }, [on, delayMs]);
  return on && shown;
}

export function SessionSubmissionStatus({
  activityStatus,
  hint,
  refresh,
}: {
  activityStatus: ShellView["shellProjection"]["activityStatus"];
  hint?: string | null;
  refresh?: { title: string; detail: string } | null;
}) {
  const status = activityStatus?.kind === "working" ? null : activityStatus;
  const quiet = !status && hint;
  const refreshing = useShownAfter(!!refresh, REFRESH_LINE_DELAY_MS);
  return (
    <div className="min-w-0" data-testid="composer-status">
      {refreshing && !status && !quiet && refresh && (
        <p
          role="status"
          title={refresh.detail}
          className="text-xs leading-4 text-muted-foreground animate-in fade-in-0 duration-200 motion-reduce:animate-none"
        >
          Updating…
        </p>
      )}
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
