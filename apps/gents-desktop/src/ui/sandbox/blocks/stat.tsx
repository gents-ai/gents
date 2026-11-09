import type { ReactNode } from "react";
import { Card, CardContent } from "@gents/ui/components/card";
import { cn } from "@gents/ui/lib/utils";

import { formatNumber } from "./format";

export type StatTone = "neutral" | "good" | "warning" | "bad";

const TONE: Record<StatTone, string> = {
  neutral: "text-muted-foreground",
  good: "text-success-foreground",
  warning: "text-warning",
  bad: "text-destructive",
};

/* the arrow carries the direction for a reader who cannot see the tone */
function arrow(delta: string | number): string {
  const text = String(delta).trim();
  if (typeof delta === "number" ? delta < 0 : /^[-−]/.test(text)) return "↓ ";
  if (typeof delta === "number" ? delta > 0 : text.startsWith("+")) return "↑ ";
  return "";
}

export interface StatProps {
  label: ReactNode;
  value: ReactNode;
  /** the change beside it: a number, or text such as "+4% this week" */
  delta?: string | number;
  /** what the change means; default "neutral" */
  tone?: StatTone;
}

export function Stat({ label, value, delta, tone = "neutral" }: StatProps) {
  return (
    <Card size="sm" data-slot="stat">
      <CardContent className="flex min-w-0 flex-col gap-1">
        <span className="truncate text-xs text-muted-foreground">{label}</span>
        <span className="truncate text-xl font-semibold text-heading">
          {typeof value === "number" ? formatNumber(value) : value}
        </span>
        {delta !== undefined && (
          <span className={cn("text-xs", TONE[tone])}>
            {arrow(delta)}
            {typeof delta === "number" ? formatNumber(Math.abs(delta)) : delta}
          </span>
        )}
      </CardContent>
    </Card>
  );
}
