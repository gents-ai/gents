import { useEffect, useRef, useState } from "react";
import { X } from "lucide-react";
import type {
  DesktopSessionSnapshot,
  SessionContextDetails,
} from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@gents/ui/components/tooltip";
import { cn } from "@gents/ui/lib/utils";

import { Popover, PopoverContent, PopoverTrigger } from "@gents/ui/components/popover";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";
import { InfoTip } from "./InfoTip";

type LastRequest = NonNullable<DesktopSessionSnapshot["context"]["lastRequest"]>;

function formatPercent(value: number) {
  return `${value.toFixed(1).replace(/\.0$/, "")}%`;
}

function trimFixed(value: number, digits: number) {
  return value.toFixed(digits).replace(/\.0$/, "");
}

function compactTokens(tokens: number) {
  if (tokens < 1000) return tokens.toLocaleString();
  const thousands = tokens / 1000;
  if (thousands < 999.5) return `${trimFixed(thousands, thousands < 99.95 ? 1 : 0)}k`;
  return `${trimFixed(tokens / 1_000_000, 1)}M`;
}

type InputPart = { label: string; tokens: number; color: string };

/* Tool definitions, the message family (one ordinal green ramp) and the other
   request layers (a gray ramp); the brand palette has no categorical chart hues. */
function inputParts(input: LastRequest, details: SessionContextDetails | null) {
  const { components } = input;
  const parts: InputPart[] = [
    { label: "Tool definitions", tokens: components.toolSchemas, color: "bg-purple" },
    ...(details
      ? [
          {
            label: "Instructions & context",
            tokens: details.systemPromptTokens,
            color: "bg-green-800 dark:bg-green-600",
          },
          {
            label: "User messages",
            tokens: details.userInputTokens,
            color: "bg-green-700 dark:bg-green-500",
          },
          {
            label: "Assistant messages",
            tokens: details.assistantTokens,
            color: "bg-green-600 dark:bg-green-400",
          },
          {
            label: "Tool calls",
            tokens: details.toolCallTokens,
            color: "bg-green-500 dark:bg-green-300",
          },
          {
            label: "Tool results",
            tokens: details.toolResultTokens,
            color: "bg-green-400 dark:bg-green-200",
          },
          {
            label: "Other message data",
            tokens: details.otherMessageTokens,
            color: "bg-green-300 dark:bg-green-100",
          },
        ]
      : [
          {
            label: "Messages & instructions",
            tokens: components.messages,
            color: "bg-green-700 dark:bg-green-500",
          },
        ]),
    {
      label: "Attached documents",
      tokens: components.documents,
      color: "bg-gray-400 dark:bg-gray-300",
    },
    {
      label: "Output schema",
      tokens: components.outputSchema,
      color: "bg-gray-500 dark:bg-gray-400",
    },
    {
      label: "Request settings",
      tokens: components.additionalParameters,
      color: "bg-gray-600 dark:bg-yellow",
    },
  ];
  return parts.filter((part) => part.tokens > 0);
}

const SEGMENT_GAP_PX = 2;

function shareLabel(tokens: number, total: number) {
  return formatPercent(total > 0 ? (tokens / total) * 100 : 0);
}

type Highlight = { active: string | null; onActive: (label: string | null) => void };

function InputBar({
  parts,
  total,
  window,
  threshold,
  active,
  onActive,
}: {
  parts: InputPart[];
  total: number;
  window: number;
  threshold: number;
} & Highlight) {
  const used = parts.reduce((sum, part) => sum + part.tokens, 0);
  const remainder = used < window;
  /* shares are laid out over the width left after the gaps, so the tick at the
     threshold lines up with the segments around it */
  const gaps = (parts.length + (remainder ? 1 : 0) - 1) * SEGMENT_GAP_PX;
  const thresholdShare = Math.min(1, threshold / window);
  let cumulative = 0;
  let gapsBeforeThreshold = 0;
  for (const part of parts) {
    cumulative += part.tokens / window;
    if (cumulative <= thresholdShare) gapsBeforeThreshold += 1;
  }
  const summary = parts
    .map((part) => `${part.label} ${part.tokens.toLocaleString()}`)
    .join(", ");
  return (
    <TooltipProvider delay={0}>
      <div
        role="img"
        aria-label={`Model input by part: ${summary || "none"}. ${window.toLocaleString()}-token context window; compaction threshold at ${threshold.toLocaleString()} tokens`}
        className="relative flex h-2 gap-0.5"
        data-testid="context-bar"
      >
        {parts.map((part) => (
          <Tooltip key={part.label}>
            <TooltipTrigger
              render={
                <span
                  className={cn(
                    "relative min-w-[3px] rounded-full transition-opacity before:absolute before:inset-x-0 before:inset-y-[-6px]",
                    part.color,
                    active && active !== part.label && "opacity-40",
                  )}
                  style={{
                    flex: `0 1 calc((100% - ${gaps}px) * ${part.tokens / window})`,
                  }}
                  data-testid="context-segment"
                  data-part={part.label}
                  data-share={(part.tokens / window).toFixed(4)}
                />
              }
              onMouseEnter={() => onActive(part.label)}
              onMouseLeave={() => onActive(null)}
            />
            <TooltipContent>
              {part.label} · {part.tokens.toLocaleString()} tokens ·{" "}
              {shareLabel(part.tokens, total)}
            </TooltipContent>
          </Tooltip>
        ))}
        {remainder && (
          <span className="min-w-0 flex-1 rounded-full bg-muted-foreground/20" />
        )}
        {threshold > 0 && (
          <span
            className="pointer-events-none absolute inset-y-[-3px] w-0 border-l border-dashed border-foreground/70"
            style={{
              left: `calc((100% - ${gaps}px) * ${thresholdShare} + ${gapsBeforeThreshold * SEGMENT_GAP_PX}px)`,
            }}
            data-testid="compaction-threshold"
            data-position={thresholdShare.toFixed(4)}
          />
        )}
      </div>
    </TooltipProvider>
  );
}

function InputLegend({
  parts,
  total,
  active,
  onActive,
}: { parts: InputPart[]; total: number } & Highlight) {
  return (
    <div
      className="-mx-1.5 text-xs"
      role="table"
      aria-label="Input token estimates"
      data-testid="input-breakdown"
    >
      <div className="sr-only" role="row">
        <span role="columnheader">Category</span>
        <span role="columnheader">Tokens</span>
        <span role="columnheader">Share</span>
      </div>
      {parts.map((part) => (
        <div
          className={cn(
            "flex items-center gap-3 rounded-md px-1.5 py-0.5 outline-none focus-visible:ring-1 focus-visible:ring-ring",
            active === part.label && "bg-accent",
          )}
          role="row"
          key={part.label}
          tabIndex={0}
          onMouseEnter={() => onActive(part.label)}
          onMouseLeave={() => onActive(null)}
          onFocus={() => onActive(part.label)}
          onBlur={() => onActive(null)}
        >
          <span
            className="flex min-w-0 flex-1 items-center gap-2 text-muted-foreground"
            role="rowheader"
          >
            <span
              aria-hidden="true"
              className={`size-2 shrink-0 rounded-full ${part.color}`}
            />
            <span className="truncate">{part.label}</span>
          </span>
          <span className="text-right tabular-nums" role="cell">
            {part.tokens.toLocaleString()}
          </span>
          <span
            className="w-11 text-right tabular-nums text-muted-foreground"
            role="cell"
          >
            {shareLabel(part.tokens, total)}
          </span>
        </div>
      ))}
    </div>
  );
}

function InputBreakdown({
  parts,
  total,
  window,
  threshold,
}: {
  parts: InputPart[];
  total: number;
  window: number;
  threshold: number;
}) {
  const [active, setActive] = useState<string | null>(null);
  return (
    <>
      <InputBar
        parts={parts}
        total={total}
        window={window}
        threshold={threshold}
        active={active}
        onActive={setActive}
      />
      <InputLegend parts={parts} total={total} active={active} onActive={setActive} />
    </>
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
  loadDetails?: (input: LastRequest) => Promise<SessionContextDetails | null>;
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
  const parts = lastRequest ? inputParts(lastRequest, details) : [];
  const loadingDetails = pendingKey === callKey && details === null;
  const notes = [
    "Latest assembled model input. Compaction is checked before each model call. The next call may differ.",
    ...(lastRequest && !details && !loadingDetails
      ? ["A detailed message breakdown is unavailable for this model call."]
      : []),
  ].join(" ");
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
        className="max-h-[min(40rem,80dvh)] w-80 max-w-[calc(100vw-2rem)] gap-2.5 overflow-y-auto p-3"
        data-testid="context-details"
        onMouseEnter={hoverOpen}
        onMouseLeave={hoverClose}
      >
        <div className="flex items-center gap-1.5">
          <p className="font-heading text-sm font-medium text-heading">Model input</p>
          {lastRequest && <InfoTip about="model input">{notes}</InfoTip>}
          <span
            className="ml-auto text-xs tabular-nums text-muted-foreground"
            title={contextLabel}
            data-testid="context-usage"
          >
            {used == null ? (
              percentageLabel
            ) : (
              <>
                {compactTokens(used)} / {compactTokens(threshold)} ·{" "}
                <span className="text-heading">{percentageLabel}</span>
              </>
            )}
          </span>
          <Button
            variant="ghost"
            size="icon-xs"
            className="-my-1 -mr-1"
            aria-label="Close context details"
            onClick={() => onOpenChange(false)}
          >
            <X />
          </Button>
        </div>
        {windowError && (
          <p role="alert" className="text-xs text-destructive">
            The configured context window can’t be used: {windowError}
          </p>
        )}
        {lastRequest ? (
          <>
            <InputBreakdown
              parts={parts}
              total={lastRequest.estimatedInputTokens}
              window={window}
              threshold={threshold}
            />
            {loadingDetails && (
              <p className="text-xs text-muted-foreground">
                Loading message breakdown…
              </p>
            )}
            <p
              className="text-xs text-muted-foreground"
              data-testid="last-context-dispatch"
            >
              {compactionReason(lastRequest.compactionReason)}
              {lastRequest.preCompactionInputTokens != null &&
                ` · ${lastRequest.preCompactionInputTokens.toLocaleString()} → ${lastRequest.estimatedInputTokens.toLocaleString()} tokens`}
            </p>
          </>
        ) : (
          <p className="text-xs text-muted-foreground">
            Model-input estimate unavailable.
          </p>
        )}
        <div
          className="space-y-1 border-t border-border pt-2 text-xs text-muted-foreground"
          data-testid="context-footer"
        >
          <p>
            <span
              title={
                windowError
                  ? undefined
                  : `Model context window: ${window.toLocaleString()} tokens`
              }
            >
              {windowError ? "Window unavailable" : `${compactTokens(window)} window`}
            </span>
            {!!sessionCompactionCount &&
              ` · ${sessionCompactionCount} ${sessionCompactionCount === 1 ? "compaction" : "compactions"}`}
          </p>
          {nextRequestSettingsChanged &&
            (context.contextWindowError ? (
              <p
                role="alert"
                className="text-destructive"
                data-testid="next-context-settings"
              >
                Next call: the configured context window can’t be used:{" "}
                {context.contextWindowError}
              </p>
            ) : (
              <p
                data-testid="next-context-settings"
                title={`Next model call: ${context.contextWindow.toLocaleString()}-token window, compaction at ${context.compactionThresholdTokens.toLocaleString()} tokens`}
              >
                Next call: {compactTokens(context.contextWindow)} window · compacts at{" "}
                {compactTokens(context.compactionThresholdTokens)}
              </p>
            ))}
          {context.compactions.length > 0 && (
            <ul
              aria-label="Recent session compactions"
              data-testid="session-compactions"
            >
              {context.compactions
                .slice(-5)
                .reverse()
                .map((entry) => (
                  <li key={entry.compactionKey} className="flex justify-between gap-3">
                    <span>
                      {entry.createdAt ? `${entry.createdAt} · ` : ""}
                      {entry.messagesCompacted == null
                        ? "Message count unavailable"
                        : `${entry.messagesCompacted.toLocaleString()} messages`}
                    </span>
                    <span className="text-right tabular-nums">
                      {entry.originalTokens == null || entry.compactedTokens == null
                        ? "Token estimates unavailable"
                        : `${entry.originalTokens.toLocaleString()} → ${entry.compactedTokens.toLocaleString()}`}
                    </span>
                  </li>
                ))}
            </ul>
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}
