/* The session's context meter: how much of the window the next request
   fills, and what compaction has done. */
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

/* how full the context is, as a stroked ring: the track is the window, the
   arc is what the conversation has used; past the compaction threshold the
   arc takes the brand color, so the number beside it need not */
function ContextRing({
  used,
  window,
  threshold,
}: {
  used: number;
  window: number;
  threshold: number;
}) {
  /* a 20px ring with a 7px radius: enough arc to read at a glance */
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
  /* in the compact header: the ring alone, the numbers in its tooltip and popover */
  compact?: boolean;
}) {
  const popover = useExclusivePopover();
  const used = Math.max(0, context.estimatedConversationTokens);
  /* the configured window as the runtime resolves it, so an edit to the
     profile shows at once; the last request's window is history. A window
     the runtime rejects is reported, never shown as the one in use. */
  const windowError = context.contextWindowError ?? null;
  const window = Math.max(1, context.contextWindow);
  const threshold = Math.max(0, context.compactionThresholdTokens);
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
      {/* the details open on hover as well as click where there is a pointer;
          on touch a tap opens them. Leaving trigger and popup both closes. */}
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
      </PopoverContent>
    </Popover>
  );
}
