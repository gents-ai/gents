/* The trace: every tool call of the current request, in order, beside the
   conversation. A call folds to a row (icon, what it did, status) and
   opens to its body: outputs, contents, the diff, a live tail while it
   runs. Nothing opens on its own. */
import { useState } from "react";
import {
  ChevronDown,
  FileText,
  PenLine,
  Search,
  SlidersHorizontal,
  Sparkles,
  Terminal,
  Wrench,
  X,
} from "lucide-react";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import type { Shell } from "@/hooks/useShell";
import { useNow } from "@/lib/clock";
import { duration } from "./tool-summary";
import { ToolBody, ToolSummary } from "./tool-views";

const ICONS: Record<string, typeof Wrench> = {
  read_doc: FileText,
  read_file: FileText,
  grep: Search,
  metrics: Search,
  edit_file: PenLine,
  bash: Terminal,
  reason: Sparkles,
  plan: Sparkles,
  delegate: Sparkles,
};
function ToolIcon({ name }: { name: string }) {
  const Icon = ICONS[name.split(".")[0]!] ?? Wrench;
  return <Icon className="size-4" />;
}

function Entry({
  tool,
  open,
  onOpenChange,
}: {
  tool: RenderedToolCallView;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const live = tool.statusKind === "running" || tool.statusKind === "held";
  return (
    <Collapsible open={open} onOpenChange={onOpenChange}>
      <CollapsibleTrigger className="flex w-full cursor-pointer items-center gap-2.5 py-2 text-left text-sm">
        <span className="grid size-4 place-items-center text-muted-foreground">
          {live ? (
            <Spinner className="text-foreground" />
          ) : (
            <ToolIcon name={tool.toolName} />
          )}
        </span>
        <ToolSummary
          tool={tool}
          withIcon
          className={cn("min-w-0 flex-1", live && "font-medium")}
        />
        {tool.statusKind === "held" && (
          <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
            awaiting approval
          </span>
        )}
        {tool.statusKind === "cancelled" && (
          <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
            cancelled
          </span>
        )}
        <Took tool={tool} />
        <ChevronDown
          className={cn(
            "size-3.5 text-muted-foreground transition-transform",
            open && "rotate-180",
          )}
        />
      </CollapsibleTrigger>
      <CollapsibleContent>
        <div className="mb-3 ml-[7px] border-l border-border pl-4">
          <ToolBody tool={tool} />
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
}

export function TracePanel({ shell, onClose }: { shell: Shell; onClose: () => void }) {
  const session = shell.selectedSession;
  const tools =
    session?.timelineItems.flatMap((i) => (i.kind === "toolGroup" ? i.tools : [])) ??
    [];
  const running = shell.selectedTrackedRequestId !== null;
  const [openKey, setOpenKey] = useState<string | null>(null);

  /* every entry starts collapsed; only the reader opens one */
  const effectiveOpen = openKey;

  return (
    <aside className="flex h-full min-h-0 flex-col rounded-2xl border border-border/60 bg-raised">
      <div className="flex h-12 items-center gap-2.5 border-b border-border/60 px-3">
        <SlidersHorizontal className="size-4 text-muted-foreground" />
        <span className="font-heading text-sm font-medium text-heading">Trace</span>
        {running && <Spinner className="ml-1 text-foreground" />}
        <Button
          variant="ghost"
          size="icon-xs"
          className="ml-auto"
          aria-label="Close trace"
          onClick={onClose}
        >
          <X />
        </Button>
      </div>
      <ScrollArea className="min-h-0 flex-1">
        <div className="px-3 py-2">
          {tools.length === 0 && (
            <p className="py-6 text-center text-sm text-muted-foreground">
              No tool calls in this request yet.
            </p>
          )}
          {tools.map((t) => (
            <Entry
              key={t.itemKey}
              tool={t}
              open={effectiveOpen === t.itemKey}
              onOpenChange={(o) => setOpenKey(o ? t.itemKey : null)}
            />
          ))}
        </div>
      </ScrollArea>
    </aside>
  );
}

/* how long a call took, or has been running: from its own timestamps,
   or the command's measured duration when the bridge reports one */
function Took({ tool }: { tool: RenderedToolCallView }) {
  const running = tool.statusKind === "running";
  const now = useNow(running);
  const started = tool.startedAt ? Date.parse(tool.startedAt) : null;
  const ms =
    tool.presentation.kind === "command" && tool.presentation.durationMs != null
      ? tool.presentation.durationMs
      : started === null
        ? null
        : running
          ? now - started
          : tool.completedAt
            ? Date.parse(tool.completedAt) - started
            : null;
  if (ms === null || ms < 0) return null;
  return (
    <span className="w-12 shrink-0 text-right font-mono text-[11px] tabular-nums text-muted-foreground">
      {duration(ms)}
    </span>
  );
}
