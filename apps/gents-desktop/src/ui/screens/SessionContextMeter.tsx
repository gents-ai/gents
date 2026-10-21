import { useRef } from "react";
import { X } from "lucide-react";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";

import { Popover, PopoverContent, PopoverTrigger } from "@gents/ui/components/popover";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";

function formatTokens(value: number) {
  if (value < 1_000) return String(value);
  const amount = value / 1_000;
  return `${amount >= 10 ? Math.round(amount) : amount.toFixed(1).replace(/\.0$/, "")}k`;
}

function compactionReason(reason: string) {
  switch (reason) {
    case "compacted":
      return "Reduced before dispatch";
    case "provider_view_repaired":
      return "Provider history repaired";
    case "below_threshold":
      return "Below the reduction threshold";
    case "compactor_unavailable":
      return "Reducer unavailable";
    default:
      return "Context reduction status unavailable";
  }
}

function ContextRing({
  used,
  window,
  threshold,
}: {
  used: number;
  window: number;
  threshold: number;
}) {
  const r = 7;
  const c = 2 * Math.PI * r;
  const share = Math.min(1, used / window);
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
}: {
  context: DesktopSessionSnapshot["context"];
  compact?: boolean;
}) {
  const popover = useExclusivePopover();
  const used = Math.max(0, context.estimatedConversationTokens);
  const windowError = context.contextWindowError ?? null;
  const window = Math.max(1, context.contextWindow);
  const threshold = Math.max(0, context.compactionThresholdTokens);
  const sessionCompactionCount = context.sessionCompactionCount;
  const hoverTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const canHover = () => globalThis.matchMedia?.("(hover: hover)").matches ?? true;
  const hoverOpen = () => {
    if (!canHover()) return;
    if (hoverTimer.current) clearTimeout(hoverTimer.current);
    hoverTimer.current = setTimeout(() => popover.onOpenChange(true), 150);
  };
  const hoverClose = () => {
    if (!canHover()) return;
    if (hoverTimer.current) clearTimeout(hoverTimer.current);
    hoverTimer.current = setTimeout(() => popover.onOpenChange(false), 200);
  };
  return (
    <Popover
      open={popover.open}
      onOpenChange={popover.onOpenChange}
      onOpenChangeComplete={popover.onOpenChangeComplete}
    >
      <PopoverTrigger
        render={
          compact ? (
            <Button
              variant="ghost"
              size="icon-xs"
              data-testid="context-meter-compact"
              aria-label={
                windowError
                  ? `Context ~${formatTokens(used)}, window unavailable`
                  : `Context ~${formatTokens(used)} of ${formatTokens(window)}`
              }
            />
          ) : (
            <Button
              variant="quiet"
              size="sm"
              data-testid="context-meter"
              className="gap-2"
            />
          )
        }
        onMouseEnter={hoverOpen}
        onMouseLeave={hoverClose}
      >
        <ContextRing
          used={windowError ? 0 : used}
          window={window}
          threshold={threshold}
        />
        {!compact && (
          <span className="tabular-nums">
            {windowError
              ? `~${formatTokens(used)} / window unavailable`
              : `~${formatTokens(used)} / ${formatTokens(window)}`}
          </span>
        )}
        <span
          className="text-xs text-muted-foreground"
          title="Durable session compactions"
        >
          {sessionCompactionCount == null
            ? "compactions unavailable"
            : `${sessionCompactionCount} ${sessionCompactionCount === 1 ? "compaction" : "compactions"}`}
        </span>
      </PopoverTrigger>
      <PopoverContent
        ref={popover.popupRef}
        aria-label="Session context details"
        align="start"
        className="w-80"
        data-testid="context-details"
        onMouseEnter={hoverOpen}
        onMouseLeave={hoverClose}
      >
        <div className="flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <p className="font-heading text-sm font-medium text-heading">
              Conversation context
            </p>
            {windowError ? (
              <p role="alert" className="mt-1 text-sm text-destructive">
                The configured context window can’t be used: {windowError}
              </p>
            ) : (
              <p className="mt-1 text-sm text-muted-foreground">
                {used.toLocaleString()} estimated tokens of {window.toLocaleString()}
              </p>
            )}
          </div>
          <Button
            variant="ghost"
            size="icon-xs"
            aria-label="Close context details"
            onClick={() => popover.onOpenChange(false)}
          >
            <X />
          </Button>
        </div>
        <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-5 gap-y-1.5 text-sm">
          {!windowError && (
            <>
              <dt className="text-muted-foreground">Compacts at</dt>
              <dd className="text-right font-mono text-xs">
                {threshold.toLocaleString()}
              </dd>
            </>
          )}
          <dt className="text-muted-foreground">Durable transcript</dt>
          <dd className="text-right font-mono text-xs">
            {context.estimatedDurableTokens.toLocaleString()}
          </dd>
        </dl>
        {context.lastRequest && (
          <section
            className="mt-4 border-t border-border pt-3"
            data-testid="last-context-dispatch"
          >
            <p className="text-sm font-medium text-heading">Latest model input</p>
            <p className="mt-1 text-sm text-muted-foreground">
              {compactionReason(context.lastRequest.compactionReason)}
            </p>
            <dl className="mt-2 grid grid-cols-[auto_1fr] gap-x-5 gap-y-1 text-sm">
              <dt className="text-muted-foreground">Input estimate</dt>
              <dd className="text-right font-mono text-xs">
                {context.lastRequest.preCompactionInputTokens == null
                  ? `${context.lastRequest.estimatedInputTokens.toLocaleString()} tokens`
                  : `${context.lastRequest.preCompactionInputTokens.toLocaleString()} → ${context.lastRequest.estimatedInputTokens.toLocaleString()} tokens`}
              </dd>
              <dt className="text-muted-foreground">Dispatch threshold</dt>
              <dd className="text-right font-mono text-xs">
                {context.lastRequest.compactionThresholdTokens.toLocaleString()} tokens
              </dd>
            </dl>
          </section>
        )}
        {context.requestReductionHistoryLoaded == null && (
          <p className="mt-3 border-t border-border pt-3 text-xs text-muted-foreground">
            Per-request model-input history is unavailable for this view.
          </p>
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
