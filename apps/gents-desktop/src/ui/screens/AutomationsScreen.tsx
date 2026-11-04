import { useState } from "react";
import {
  ArrowLeft,
  Hand,
  ListFilter,
  Play,
  Plus,
  Search,
  Timer,
  X,
  Zap,
} from "lucide-react";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { Input } from "@gents/ui/components/input";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { cn } from "@gents/ui/lib/utils";
import { href, navigate } from "@/lib/router";
import { SEARCH_FIELD } from "@/lib/textAssist";
import { useSelectedNode } from "@/hooks/useClient";
import { NewAutomationDialog } from "./agent/NewAutomationDialog";
import { AutomationDetail } from "./automations/AutomationDetail";
import {
  automationsOf,
  inTab,
  matches,
  tabOrder,
  type Automation,
  type AutomationTab,
} from "./automations/model";
import { TEMPLATES, type AutomationTemplate } from "./automations/templates";
import { span, useMinute, when } from "./time";

const FILTERS: { key: AutomationTab; label: string }[] = [
  { key: "all", label: "All" },
  { key: "next", label: "Next up" },
  { key: "attention", label: "Needs attention" },
  { key: "off", label: "Off" },
];

/** What runs on its own on the active node: the automations as a list
    beside the one open, or, with none open, ways to start one. */
export function AutomationsScreen({ item }: { item?: string }) {
  const node = useSelectedNode();
  const now = useMinute();
  const [filter, setFilter] = useState<AutomationTab>("all");
  const [query, setQuery] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [template, setTemplate] = useState<AutomationTemplate | null>(null);
  const start = (from: AutomationTemplate | null) => {
    setTemplate(from);
    setCreating(true);
  };
  const all = node ? automationsOf(node) : [];
  const shown = tabOrder(
    all.filter((a) => matches(a, query ?? "") && inTab(a, filter, now)),
    filter,
  );
  const scheduled = shown.filter((a) => a.kind !== "manual");
  const byHand = shown.filter((a) => a.kind === "manual");
  const open = item ? (all.find((a) => a.id === item) ?? null) : null;
  const name = node ? (node.node.displayName ?? node.label) : "this node";

  return (
    <div className="flex h-full min-h-0" data-testid="automations-screen">
      <aside
        className={cn(
          "flex w-full min-w-0 flex-col border-border/60 md:w-72 md:shrink-0 md:border-r",
          open && "max-md:hidden",
        )}
      >
        <div className="flex items-center gap-1 px-4 pt-5 pb-2">
          <h1 className="min-w-0 flex-1 truncate font-heading text-lg font-medium text-heading">
            Automations
          </h1>
          <DropdownMenu>
            <DropdownMenuTrigger
              render={
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label="Show"
                  className={cn(
                    "text-muted-foreground",
                    filter !== "all" && "text-foreground",
                  )}
                />
              }
            >
              <ListFilter />
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-48">
              <DropdownMenuRadioGroup
                value={filter}
                onValueChange={(value) => setFilter(value as AutomationTab)}
              >
                {FILTERS.map((f) => (
                  <DropdownMenuRadioItem key={f.key} value={f.key}>
                    <span className="flex-1">{f.label}</span>
                    <span className="text-xs text-muted-foreground tabular-nums">
                      {all.filter((a) => inTab(a, f.key, now)).length}
                    </span>
                  </DropdownMenuRadioItem>
                ))}
              </DropdownMenuRadioGroup>
            </DropdownMenuContent>
          </DropdownMenu>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Search automations"
            aria-expanded={query !== null}
            className={cn("text-muted-foreground", query !== null && "text-foreground")}
            onClick={() => setQuery(query === null ? "" : null)}
          >
            <Search />
          </Button>
        </div>
        {query !== null && (
          <div className="relative px-3 pb-2">
            <Input
              autoFocus
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => e.key === "Escape" && setQuery(null)}
              {...SEARCH_FIELD}
              placeholder="Search automations"
              className="h-8 w-full pr-8"
              aria-label="Search automations"
            />
            <Button
              variant="ghost"
              size="icon-xs"
              aria-label="Clear search"
              className="absolute top-1/2 right-4 -translate-y-1/2 text-muted-foreground"
              onClick={() => setQuery(null)}
            >
              <X />
            </Button>
          </div>
        )}
        <ScrollArea className="min-h-0 flex-1">
          <div className="grid gap-px px-2 pb-4">
            {node && (
              <button
                type="button"
                onClick={() => start(null)}
                className="flex h-9 cursor-pointer items-center gap-2.5 rounded-lg px-2 text-sm transition-colors outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring/50"
              >
                <Plus className="size-4 text-muted-foreground" /> New automation
              </button>
            )}
            {filter !== "all" && (
              <button
                type="button"
                onClick={() => setFilter("all")}
                className="mx-2 mt-1 mb-1 flex w-fit cursor-pointer items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
              >
                Showing {FILTERS.find((f) => f.key === filter)!.label.toLowerCase()}
                <X className="size-3" />
              </button>
            )}
            {scheduled.map((a) => (
              <Row key={a.id} automation={a} now={now} current={a.id === item} />
            ))}
            {byHand.length > 0 && (
              <h2 className="mt-3 mb-1 px-2 font-mono text-[11px] tracking-wide text-muted-foreground uppercase">
                Run by hand
              </h2>
            )}
            {byHand.map((a) => (
              <Row key={a.id} automation={a} now={now} current={a.id === item} />
            ))}
            {all.length > 0 && shown.length === 0 && (
              <p className="px-2 pt-2 text-sm text-muted-foreground">
                Nothing matches.
              </p>
            )}
            {all.length === 0 && (
              <p className="px-2 pt-2 text-sm text-muted-foreground">
                No automations yet.
              </p>
            )}
          </div>
        </ScrollArea>
      </aside>
      <ScrollArea className={cn("min-h-0 min-w-0 flex-1", !open && "max-md:hidden")}>
        <div className="mx-auto max-w-3xl px-6 py-8">
          {open && node ? (
            <>
              <a
                href={href({ name: "automations" })}
                className="mb-4 inline-flex items-center gap-1 text-xs text-muted-foreground hover:text-foreground md:hidden"
              >
                <ArrowLeft className="size-3" /> Automations
              </a>
              <AutomationDetail node={node} automation={open} />
            </>
          ) : (
            <Start node={name} onUse={start} />
          )}
        </div>
      </ScrollArea>
      {node && (
        <NewAutomationDialog
          deployment={node}
          open={creating}
          onOpenChange={setCreating}
          template={template}
          onCreated={(triggerId) => navigate({ name: "automations", item: triggerId })}
        />
      )}
    </div>
  );
}

const ICON = { schedule: Timer, event: Zap, manual: Hand };

/* what a row says under its name: why it can't run, how its last run went,
   or when it runs next */
function statusOf(a: Automation, now: number): { text: string; warn: boolean } {
  if (!a.on) return { text: "Off", warn: false };
  if (!a.readiness.ok) return { text: a.readiness.reason, warn: true };
  if (a.lastFailed) return { text: `Failed ${when(a.lastRunAt, now)} ago`, warn: true };
  const next = a.nextRunAt ? Date.parse(a.nextRunAt) : null;
  if (next && next > now) return { text: `Next in ${span(next - now)}`, warn: false };
  if (a.lastRunAt) return { text: `Ran ${when(a.lastRunAt, now)} ago`, warn: false };
  return { text: a.when, warn: false };
}

function Row({
  automation: a,
  now,
  current,
}: {
  automation: Automation;
  now: number;
  current: boolean;
}) {
  const Icon = ICON[a.kind];
  const status = statusOf(a, now);
  return (
    <a
      href={href({ name: "automations", item: a.id })}
      aria-current={current ? "page" : undefined}
      className={cn(
        "flex items-start gap-2.5 rounded-lg px-2 py-2 transition-colors outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring/50",
        current && "bg-accent",
      )}
    >
      <Icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
      <span className="min-w-0 flex-1">
        <span
          className={cn("block truncate text-sm", !a.on && "text-muted-foreground")}
        >
          {a.name}
        </span>
        <span
          className={cn(
            "block truncate text-xs",
            status.warn ? "text-warning" : "text-muted-foreground",
          )}
        >
          {status.text}
        </span>
      </span>
    </a>
  );
}

/* with nothing open: what an automation is, and ready-made ones to start
   from, each opening New automation filled in */
function Start({
  node,
  onUse,
}: {
  node: string;
  onUse: (t: AutomationTemplate) => void;
}) {
  return (
    <section aria-label="Start an automation" className="grid gap-8 pt-6">
      <div className="grid justify-items-center gap-2 text-center">
        <span className="grid size-12 place-items-center rounded-2xl bg-raised text-muted-foreground">
          <Play className="size-6" />
        </span>
        <h2 className="mt-2 font-heading text-lg font-medium text-heading">
          Automate work on {node}
        </h2>
        <p className="max-w-md text-sm text-muted-foreground">
          An agent can run a task for you on a schedule, when something happens, or
          whenever you start it.
        </p>
      </div>
      <div className="grid gap-3 sm:grid-cols-2" aria-label="Templates">
        {TEMPLATES.map((t) => (
          <button
            key={t.id}
            type="button"
            onClick={() => onUse(t)}
            className="flex cursor-pointer items-start gap-3 rounded-xl border border-border/60 px-4 py-3.5 text-left transition-colors outline-none hover:bg-accent/50 focus-visible:ring-2 focus-visible:ring-ring/50"
          >
            <t.icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
            <span className="grid gap-1">
              <span className="text-sm font-medium">{t.name}</span>
              <span className="text-xs text-muted-foreground">{t.blurb}</span>
            </span>
          </button>
        ))}
      </div>
    </section>
  );
}
