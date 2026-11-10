import type { ReactNode } from "react";
import { Inbox } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { activityOf, useFleet } from "@/hooks/useFleet";
import { Hint } from "@/screens/Hint";
import { counted } from "@/screens/NodeFacts";

/** What a node has going on, as marks with counts: its running sessions
    with the session rows' spinner, its open mailbox items with the nav's
    inbox. Each mark is named in words for its tooltip and screen readers;
    an idle node shows nothing. */
export function NodeActivity({
  nodeDid,
  className,
}: {
  nodeDid: string;
  className?: string;
}) {
  const { running, waiting } = useFleet(useShallow((s) => activityOf(s, nodeDid)));
  if (!running && !waiting) return null;
  return (
    <span
      className={cn(
        "flex shrink-0 items-center gap-2.5 text-xs text-muted-foreground tabular-nums",
        className,
      )}
    >
      {running > 0 && (
        <Mark label={counted(running, "running session")}>
          <Spinner className="text-foreground" />
          {running}
        </Mark>
      )}
      {waiting > 0 && (
        <Mark label={counted(waiting, "mailbox item")}>
          <Inbox className="size-3.5" />
          {waiting}
        </Mark>
      )}
    </span>
  );
}

function Mark({ label, children }: { label: string; children: ReactNode }) {
  return (
    <Hint label={label}>
      <span role="img" aria-label={label} className="flex items-center gap-1">
        {children}
      </span>
    </Hint>
  );
}
