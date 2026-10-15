import type { ReactNode } from "react";
import { ChevronRight } from "lucide-react";
import { cn } from "@gents/ui/lib/utils";

/**
 * A section folded away behind its title. A native `<details>`, so a field
 * inside it can be revealed by opening the fold (`focusFirstProblem`), drawn
 * with a chevron in place of the browser's marker.
 */
export function Disclosure({
  summary,
  className,
  summaryClassName,
  children,
}: {
  summary: ReactNode;
  className?: string;
  summaryClassName?: string;
  children: ReactNode;
}) {
  return (
    <details className={cn("group", className)}>
      <summary
        className={cn(
          "flex cursor-pointer list-none items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground [&::-webkit-details-marker]:hidden",
          summaryClassName,
        )}
      >
        <ChevronRight
          aria-hidden
          className="size-3.5 shrink-0 transition-transform group-open:rotate-90"
        />
        {summary}
      </summary>
      {children}
    </details>
  );
}
