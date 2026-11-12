/* What a data block shows while it has no data to show. */
import { useEffect, useState, type ReactNode } from "react";
import {
  Alert,
  AlertAction,
  AlertDescription,
  AlertTitle,
} from "@gents/ui/components/alert";
import { Button } from "@gents/ui/components/button";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@gents/ui/components/empty";
import { Skeleton } from "@gents/ui/components/skeleton";

export interface LoadingStateProps {
  /** what a screen reader hears; default "Loading" */
  label?: string;
}

export function LoadingState({ label = "Loading" }: LoadingStateProps) {
  return (
    <div
      role="status"
      aria-label={label}
      data-slot="loading-state"
      className="flex flex-col gap-3 p-4"
    >
      <Skeleton className="h-4 w-1/3" />
      <Skeleton className="h-24 w-full" />
      <Skeleton className="h-4 w-2/3" />
    </div>
  );
}

export interface EmptyStateProps {
  /** default "Nothing to show" */
  title?: ReactNode;
  message?: ReactNode;
  /** a button that gets the user out of the empty state */
  action?: ReactNode;
}

export function EmptyState({
  title = "Nothing to show",
  message,
  action,
}: EmptyStateProps) {
  return (
    <Empty data-slot="empty-state">
      <EmptyHeader>
        <EmptyTitle className="text-base">{title}</EmptyTitle>
        {message && <EmptyDescription>{message}</EmptyDescription>}
      </EmptyHeader>
      {action && <EmptyContent>{action}</EmptyContent>}
    </Empty>
  );
}

export interface ErrorStateProps {
  /** what went wrong; `useQuery`'s `error` as it stands */
  message: ReactNode;
  /** default "Could not load this" */
  title?: ReactNode;
  /** draws a Retry button; `useQuery`'s `refresh` as it stands */
  onRetry?: () => void;
}

export function ErrorState({
  message,
  title = "Could not load this",
  onRetry,
}: ErrorStateProps) {
  return (
    <Alert variant="destructive" data-slot="error-state">
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription>{message}</AlertDescription>
      {onRetry && (
        <AlertAction>
          <Button size="xs" variant="outline" onClick={onRetry}>
            Retry
          </Button>
        </AlertAction>
      )}
    </Alert>
  );
}

const RELATIVE = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
const STEPS: Array<[Intl.RelativeTimeFormatUnit, number]> = [
  ["day", 86_400_000],
  ["hour", 3_600_000],
  ["minute", 60_000],
];

function ago(at: number, now: number): string {
  const elapsed = now - at;
  for (const [unit, ms] of STEPS) {
    if (elapsed >= ms) return RELATIVE.format(-Math.floor(elapsed / ms), unit);
  }
  return "just now";
}

export interface LastUpdatedProps {
  /** when the data was read; `useQuery`'s `updatedAt` as it stands */
  at: number | string | Date | null | undefined;
  /** says a new read is running; `useQuery`'s `loading` as it stands */
  loading?: boolean;
}

export function LastUpdated({ at, loading = false }: LastUpdatedProps) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(timer);
  }, []);
  const time = at === null || at === undefined ? Number.NaN : new Date(at).getTime();
  if (Number.isNaN(time)) {
    return (
      <span data-slot="last-updated" className="text-xs text-muted-foreground">
        {loading ? "Loading…" : "Not updated yet"}
      </span>
    );
  }
  return (
    <time
      data-slot="last-updated"
      dateTime={new Date(time).toISOString()}
      title={new Date(time).toLocaleString()}
      className="text-xs text-muted-foreground"
    >
      {loading ? "Updating…" : `Updated ${ago(time, Math.max(now, time))}`}
    </time>
  );
}
