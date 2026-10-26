import { useEffect, useRef, useState } from "react";
import { X } from "lucide-react";
import type {
  DesktopSessionSnapshot,
  SessionContextDetails,
} from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";

import { Popover, PopoverContent, PopoverTrigger } from "@gents/ui/components/popover";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";

function formatPercent(value: number) {
  return `${value.toFixed(1).replace(/\.0$/, "")}%`;
}

function InputBreakdown({
  input,
  details,
  loading,
}: {
  input: NonNullable<DesktopSessionSnapshot["context"]["lastRequest"]>;
  details: SessionContextDetails | null;
  loading: boolean;
}) {
  const { components } = input;
  const rows: [string, number][] = details
    ? [
        ["Instructions & context", details.systemPromptTokens],
        ["User messages", details.userInputTokens],
        ["Assistant messages", details.assistantTokens],
        ["Tool calls", details.toolCallTokens],
        ["Tool results", details.toolResultTokens],
        ["Other message data", details.otherMessageTokens],
      ]
    : [["Messages & instructions", components.messages]];
  rows.push(
    ["Tool definitions", components.toolSchemas],
    ["Attached documents", components.documents],
    ["Request settings", components.additionalParameters],
    ["Output schema", components.outputSchema],
  );
  return (
    <section className="mt-4 border-t border-border pt-3" data-testid="input-breakdown">
      <p className="text-sm font-medium text-heading">Input breakdown</p>
      <div
        className="mt-2 grid grid-cols-[minmax(0,1fr)_auto_auto] gap-x-3 gap-y-2 text-sm"
        role="table"
        aria-label="Input token estimates"
      >
        <div className="contents text-xs text-muted-foreground" role="row">
          <span role="columnheader">Category</span>
          <span role="columnheader" className="text-right">
            Tokens
          </span>
          <span role="columnheader" className="text-right">
            Share
          </span>
        </div>
        {rows
          .filter(([, tokens]) => tokens > 0)
          .map(([label, tokens]) => (
            <div className="contents" role="row" key={label}>
              <span className="text-muted-foreground" role="rowheader">
                {label}
              </span>
              <span className="text-right font-mono text-xs" role="cell">
                {tokens.toLocaleString()}
              </span>
              <span
                className="text-right text-xs tabular-nums text-muted-foreground"
                role="cell"
              >
                {formatPercent(
                  input.estimatedInputTokens > 0
                    ? (tokens / input.estimatedInputTokens) * 100
                    : 0,
                )}
              </span>
            </div>
          ))}
      </div>
      {!details && (
        <p className="mt-2 text-xs text-muted-foreground">
          {loading
            ? "Loading message breakdown…"
            : "A detailed message breakdown is unavailable for this model call."}
        </p>
      )}
    </section>
  );
}

function compactionReason(reason: string) {
  switch (reason) {
    case "compacted":
      return "Input compacted before this model call";
    case "provider_view_repaired":
      return "Provider history repaired";
    case "below_threshold":
      return "No compaction needed for this model call";
    case "compactor_unavailable":
      return "Automatic compaction unavailable for this model call";
    default:
      return "Compaction status unavailable";
  }
}

function ContextRing({ used, threshold }: { used: number; threshold: number }) {
  const r = 7;
  const c = 2 * Math.PI * r;
  const share = threshold > 0 ? Math.min(1, used / threshold) : 0;
  const nearCompaction = threshold > 0 && used >= threshold;
  return (
    <svg
      viewBox="0 0 20 20"
      className="size-5 shrink-0 -rotate-90"
      aria-hidden="true"
      data-testid="context-ring"
      data-share={share.toFixed(2)}
    >
      <circle
        cx="10"
        cy="10"
        r={r}
        fill="none"
        stroke="currentColor"
        strokeOpacity="0.2"
        strokeWidth="2"
      />
      <circle
        cx="10"
        cy="10"
        r={r}
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeDasharray={`${share * c} ${c}`}
        className={nearCompaction ? "text-brand" : ""}
      />
    </svg>
  );
}

export function SessionContext({
  context,
  compact = false,
  loadDetails,
}: {
  context: DesktopSessionSnapshot["context"];
  compact?: boolean;
  loadDetails?: (
    input: NonNullable<DesktopSessionSnapshot["context"]["lastRequest"]>,
  ) => Promise<SessionContextDetails | null>;
}) {
  const popover = useExclusivePopover();
  const lastRequest = context.lastRequest;
  const callKey = lastRequest
    ? JSON.stringify([
        lastRequest.requestId,
        lastRequest.callId,
        lastRequest.turnIndex,
        lastRequest.attempt,
      ])
    : null;
  const [loaded, setLoaded] = useState<{
    key: string;
    loader: typeof loadDetails;
    details: SessionContextDetails | null;
  } | null>(null);
  const [pendingKey, setPendingKey] = useState<string | null>(null);
  useEffect(() => {
    if (!popover.open || !lastRequest || !loadDetails || !callKey) return;
    let current = true;
    setPendingKey(callKey);
    loadDetails(lastRequest)
      .then(
        (details) => {
          if (current) setLoaded({ key: callKey, loader: loadDetails, details });
        },
        () => {
          if (current) setLoaded({ key: callKey, loader: loadDetails, details: null });
        },
      )
      .finally(() => {
        if (current) setPendingKey(null);
      });
    return () => {
      current = false;
    };
  }, [popover.open, callKey, loadDetails]);
  const details =
    loaded?.key === callKey && loaded.loader === loadDetails ? loaded.details : null;
  const used = lastRequest ? Math.max(0, lastRequest.estimatedInputTokens) : null;
  const windowError = lastRequest ? null : (context.contextWindowError ?? null);
  const window = Math.max(1, lastRequest?.contextWindow ?? context.contextWindow);
  const threshold = Math.max(
    0,
    lastRequest?.compactionThresholdTokens ?? context.compactionThresholdTokens,
  );
  const percentage = used != null && threshold > 0 ? (used / threshold) * 100 : null;
  const percentageLabel = percentage == null ? "—" : formatPercent(percentage);
  const nextRequestSettingsChanged =
    lastRequest &&
    (context.contextWindowError ||
      context.contextWindow !== lastRequest.contextWindow ||
      context.compactionThresholdTokens !== lastRequest.compactionThresholdTokens);
  const sessionCompactionCount = context.sessionCompactionCount;
  const contextLabel =
    percentage == null
      ? "Model-input estimate unavailable"
      : `${percentageLabel} of compaction threshold; ${used!.toLocaleString()} of ${threshold.toLocaleString()} estimated tokens`;
  const compactionLabel =
    sessionCompactionCount == null
      ? "Session compactions unavailable"
      : `${sessionCompactionCount} session ${sessionCompactionCount === 1 ? "compaction" : "compactions"}`;
  const hoverTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  /* A press pins the details open; only hover previews close on pointer exit */
  const pinned = useRef(false);
  const canHover = () => globalThis.matchMedia?.("(hover: hover)").matches ?? true;
  const hoverOpen = () => {
    if (!canHover()) return;
    if (hoverTimer.current) clearTimeout(hoverTimer.current);
    hoverTimer.current = setTimeout(() => popover.onOpenChange(true), 150);
  };
  const hoverClose = () => {
    if (!canHover() || pinned.current) return;
    if (hoverTimer.current) clearTimeout(hoverTimer.current);
    hoverTimer.current = setTimeout(() => onOpenChange(false), 200);
  };
  const onOpenChange = (next: boolean) => {
    if (!next) pinned.current = false;
    popover.onOpenChange(next);
  };
  return (
    <Popover
      open={popover.open}
      onOpenChange={onOpenChange}
      onOpenChangeComplete={popover.onOpenChangeComplete}
    >
      <PopoverTrigger
        render={
          compact ? (
            <Button
              variant="ghost"
              size="xs"
              data-testid="context-meter-compact"
              className="gap-1 px-1.5"
              aria-label={`${contextLabel}; ${compactionLabel}`}
            />
          ) : (
            <Button
              variant="quiet"
              size="sm"
              data-testid="context-meter"
              className="gap-2"
              aria-label={`${contextLabel}; ${compactionLabel}`}
            />
          )
        }
        onClick={() => {
          if (hoverTimer.current) clearTimeout(hoverTimer.current);
          pinned.current = true;
        }}
        /* a move, not an enter: the meter can render under a resting pointer */
        onMouseMove={() => !popover.open && hoverOpen()}
        onMouseLeave={hoverClose}
      >
        <ContextRing used={used ?? 0} threshold={threshold} />
        <span className="tabular-nums">{percentageLabel}</span>
        {!compact && (
          <span className="text-xs text-muted-foreground" title={compactionLabel}>
            {sessionCompactionCount == null
              ? "compactions unavailable"
              : `${sessionCompactionCount} ${sessionCompactionCount === 1 ? "compaction" : "compactions"}`}
          </span>
        )}
      </PopoverTrigger>
      <PopoverContent
        ref={popover.popupRef}
        aria-label="Session context details"
        align="start"
        className="max-h-[min(40rem,80dvh)] w-96 max-w-[calc(100vw-2rem)] overflow-y-auto"
        data-testid="context-details"
        onMouseEnter={hoverOpen}
        onMouseLeave={hoverClose}
      >
        <div className="flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <p className="font-heading text-sm font-medium text-heading">Model input</p>
            <p className="mt-1 text-2xl font-medium tabular-nums text-heading">
              {percentageLabel}
              <span className="ml-2 text-sm font-normal text-muted-foreground">
                of compaction threshold
              </span>
            </p>
            {used != null && (
              <p className="mt-1 text-sm text-muted-foreground">
                {used.toLocaleString()} / {threshold.toLocaleString()} estimated tokens
              </p>
            )}
            {windowError && (
              <p role="alert" className="mt-1 text-sm text-destructive">
                The configured context window can’t be used: {windowError}
              </p>
            )}
          </div>
          <Button
            variant="ghost"
            size="icon-xs"
            aria-label="Close context details"
            onClick={() => onOpenChange(false)}
          >
            <X />
          </Button>
        </div>
        {lastRequest ? (
          <>
            <section className="mt-3 text-sm" data-testid="last-context-dispatch">
              <p className="text-muted-foreground">
                {compactionReason(lastRequest.compactionReason)}
              </p>
              {lastRequest.preCompactionInputTokens != null && (
                <p className="mt-1 text-xs text-muted-foreground">
                  Before → after compaction:{" "}
                  {lastRequest.preCompactionInputTokens.toLocaleString()} →{" "}
                  {lastRequest.estimatedInputTokens.toLocaleString()} tokens
                </p>
              )}
            </section>
            <InputBreakdown
              input={lastRequest}
              details={details}
              loading={pendingKey === callKey && details === null}
            />
            <p className="mt-3 text-xs text-muted-foreground">
              Latest assembled model input. Compaction is checked before each model
              call. The next call may differ.
            </p>
          </>
        ) : (
          <p className="mt-3 text-xs text-muted-foreground">
            Model-input estimate unavailable.
          </p>
        )}
        <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-5 gap-y-1.5 border-t border-border pt-3 text-sm">
          <dt className="text-muted-foreground">Model context window</dt>
          <dd className="text-right font-mono text-xs">
            {windowError ? "Unavailable" : window.toLocaleString()}
          </dd>
          <dt className="text-muted-foreground">Session compactions</dt>
          <dd className="text-right font-mono text-xs">
            {sessionCompactionCount ?? "Unavailable"}
          </dd>
        </dl>
        {nextRequestSettingsChanged && (
          <section
            className="mt-3 border-t border-border pt-3"
            data-testid="next-context-settings"
          >
            <p className="text-sm font-medium text-heading">Next model call</p>
            {context.contextWindowError ? (
              <p role="alert" className="mt-1 text-sm text-destructive">
                The configured context window can’t be used:{" "}
                {context.contextWindowError}
              </p>
            ) : (
              <p className="mt-1 text-xs text-muted-foreground">
                Window: {context.contextWindow.toLocaleString()} tokens. Compaction
                threshold: {context.compactionThresholdTokens.toLocaleString()} tokens.
              </p>
            )}
          </section>
        )}
        {context.compactions.length > 0 && (
          <section
            className="mt-4 border-t border-border pt-3"
            data-testid="session-compactions"
          >
            <p className="text-sm font-medium text-heading">Session compactions</p>
            <ul className="mt-2 space-y-2 text-sm">
              {context.compactions
                .slice(-5)
                .reverse()
                .map((entry) => (
                  <li key={entry.compactionKey} className="flex justify-between gap-3">
                    <span className="text-muted-foreground">
                      {entry.createdAt ? `${entry.createdAt} · ` : ""}
                      {entry.messagesCompacted == null
                        ? "Message count unavailable"
                        : `${entry.messagesCompacted.toLocaleString()} messages`}
                    </span>
                    <span className="text-right font-mono text-xs">
                      {entry.originalTokens == null || entry.compactedTokens == null
                        ? "Token estimates unavailable"
                        : `${entry.originalTokens.toLocaleString()} → ${entry.compactedTokens.toLocaleString()}`}
                    </span>
                  </li>
                ))}
            </ul>
          </section>
        )}
      </PopoverContent>
    </Popover>
  );
}
