import { cn } from "@gents/ui/lib/utils";
import { href } from "@/lib/router";

export function Mark({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 810 579"
      fill="currentColor"
      className={className}
      aria-hidden="true"
    >
      <path d="M346.89 462.851H694.215V578.571H346.831V462.851H231.09V347.146H346.89V462.851Z" />
      <path d="M810 347.146H578.416V231.426H810V347.146Z" />
      <path d="M346.875 115.72V231.426H0V115.706L346.875 115.72Z" />
      <path d="M694.215 0V115.706L346.875 115.72L346.89 0H694.215Z" />
    </svg>
  );
}

export function MarkLink({
  className,
  current = false,
}: {
  className?: string;
  /** the Nodes page, where the mark leads, is open: ringed as the rail's
      node is on its own pages */
  current?: boolean;
}) {
  return (
    <a
      href={href({ name: "agents" })}
      aria-label="Nodes"
      aria-current={current ? "page" : undefined}
      className={cn(
        "grid size-7 shrink-0 place-items-center rounded-md bg-ink text-background ring-offset-2 ring-offset-background transition-shadow",
        current && "ring-2 ring-ink",
        className,
      )}
    >
      <Mark className="h-3" />
    </a>
  );
}
