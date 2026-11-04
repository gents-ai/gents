import { useState } from "react";
import { Hand, Plus, Search, Timer, Workflow, X, Zap } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { Switch } from "@gents/ui/components/switch";
import { Tabs, TabsList, TabsTrigger } from "@gents/ui/components/tabs";
import { cn } from "@gents/ui/lib/utils";
import { navigate } from "@/lib/router";
import { SEARCH_FIELD } from "@/lib/textAssist";
import { useApp } from "@/app/AppContext";
import { useSelectedNode } from "@/hooks/useClient";
import type { NodeView } from "../../hooks/fleetStore";
import { setEnabled } from "./agent/enabled";
import { NewAutomationDialog } from "./agent/NewAutomationDialog";
import { AutomationSheet } from "./automations/AutomationSheet";
import {
  automationsOf,
  inTab,
  MANUAL,
  matches,
  tabOrder,
  type Automation,
  type AutomationTab,
} from "./automations/model";
import { span, useMinute, when } from "./time";

const TABS: { key: AutomationTab; label: string }[] = [
  { key: "all", label: "All" },
  { key: "next", label: "Next up" },
  { key: "attention", label: "Needs attention" },
  { key: "off", label: "Off" },
];

/** What runs on its own on the active node: each automation as a sentence,
    whether it can run, when it runs next and how it last went. */
export function AutomationsScreen({ item }: { item?: string }) {
  const node = useSelectedNode();
  const now = useMinute();
  const [tab, setTab] = useState<AutomationTab>("all");
  const [query, setQuery] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const all = node ? automationsOf(node) : [];
  const searched = all.filter((a) => matches(a, query ?? ""));
  const shown = tabOrder(
    searched.filter((a) => inTab(a, tab, now)),
    tab,
  );
  const scheduled = shown.filter((a) => a.kind !== "manual");
  const byHand = shown.filter((a) => a.kind === "manual");
  const open = item ? (all.find((a) => a.id === item) ?? null) : null;
  const name = node ? (node.node.displayName ?? node.label) : null;

  return (
    <ScrollArea className="h-full" data-testid="automations-screen">
      <div className="mx-auto max-w-page px-6 py-6">
        <header className="mb-4 flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <h1 className="font-heading text-lg font-medium text-heading">
              Automations
            </h1>
            <p className="mt-0.5 text-sm text-muted-foreground">
              {name ? `What runs on its own on ${name}` : "No node is selected"}
            </p>
          </div>
          {node && (
            <Button variant="brand" size="sm" onClick={() => setCreating(true)}>
              <Plus /> New automation
            </Button>
          )}
        </header>
        {all.length > 0 && (
          <>
            <div className="flex items-center gap-2">
              <div className="min-w-0 flex-1 overflow-x-auto py-1">
                <Tabs
                  value={tab}
                  onValueChange={(value) => setTab(value as AutomationTab)}
                  className="w-max"
                >
                  <TabsList
                    variant="line"
                    aria-label="Automations"
                    className="min-h-11"
                  >
                    {TABS.map((t) => (
                      <TabsTrigger key={t.key} value={t.key} className={TAB}>
                        {t.label}{" "}
                        <Count
                          n={searched.filter((a) => inTab(a, t.key, now)).length}
                        />
                      </TabsTrigger>
                    ))}
                  </TabsList>
                </Tabs>
              </div>
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label="Search automations"
                aria-expanded={query !== null}
                className={cn(
                  "ml-auto shrink-0 text-muted-foreground",
                  query !== null && "text-foreground",
                )}
                onClick={() => setQuery(query === null ? "" : null)}
              >
                <Search />
              </Button>
            </div>
            {query !== null && (
              <div className="relative mt-3">
                <Input
                  autoFocus
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  onKeyDown={(e) => e.key === "Escape" && setQuery(null)}
                  {...SEARCH_FIELD}
                  placeholder="Search names, what they do and when"
                  className="h-9 w-full pr-9"
                  aria-label="Search automations"
                />
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label="Clear search"
                  className="absolute top-1/2 right-1 -translate-y-1/2 text-muted-foreground"
                  onClick={() => setQuery(null)}
                >
                  <X />
                </Button>
              </div>
            )}
            {node && scheduled.length > 0 && (
              <List
                node={node}
                list={scheduled}
                now={now}
                label="Scheduled and on events"
              />
            )}
            {node && byHand.length > 0 && (
              <List node={node} list={byHand} now={now} label="Run by hand" heading />
            )}
            {shown.length === 0 && (
              <p
                className="mt-8 text-sm text-muted-foreground"
                data-testid="automations-tab-empty"
              >
                {searched.length ? "Nothing here." : "Nothing matches."}
              </p>
            )}
          </>
        )}
        {node && all.length === 0 && (
          <div className="grid min-h-[50vh] place-items-center">
            <div className="text-center">
              <Workflow className="mx-auto size-6 text-muted-foreground" />
              <p className="mt-3 font-heading text-lg font-medium text-heading">
                Nothing runs on its own yet
              </p>
              <p className="mx-auto mt-2 max-w-sm text-sm text-muted-foreground">
                An automation runs a task with an agent on a schedule, when something
                happens, or when you start it.
              </p>
              <Button
                variant="brand"
                className="mt-5"
                onClick={() => setCreating(true)}
              >
                <Plus /> New automation
              </Button>
            </div>
          </div>
        )}
      </div>
      {node && (
        <NewAutomationDialog
          deployment={node}
          open={creating}
          onOpenChange={setCreating}
          onCreated={(triggerId) => navigate({ name: "automations", item: triggerId })}
        />
      )}
      {node && (
        <AutomationSheet
          node={node}
          automation={open}
          onClose={() => navigate({ name: "automations" })}
        />
      )}
    </ScrollArea>
  );
}

function List({
  node,
  list,
  now,
  label,
  heading = false,
}: {
  node: NodeView;
  list: Automation[];
  now: number;
  label: string;
  heading?: boolean;
}) {
  return (
    <section className="mt-5" aria-label={label}>
      {heading && (
        <h2 className="mb-1 px-2 font-mono text-[11px] tracking-wide text-muted-foreground uppercase">
          {label}
        </h2>
      )}
      <ul data-testid="automations-list">
        {list.map((a) => (
          <AutomationRow key={a.id} node={node} automation={a} now={now} />
        ))}
      </ul>
    </section>
  );
}

const ICON = { schedule: Timer, event: Zap, manual: Hand };

function AutomationRow({
  node,
  automation: a,
  now,
}: {
  node: NodeView;
  automation: Automation;
  now: number;
}) {
  const {
    actions: { changeConfig },
  } = useApp();
  const Icon = ICON[a.kind];
  const next = a.on && a.nextRunAt ? Date.parse(a.nextRunAt) : null;
  const status = !a.readiness.ok
    ? { text: a.readiness.reason, tone: a.on ? "warning" : "muted" }
    : a.lastFailed
      ? { text: `Last run failed ${when(a.lastRunAt, now)} ago`, tone: "warning" }
      : next && next > now
        ? { text: `Next in ${span(next - now)}`, tone: "muted" }
        : a.lastRunAt
          ? { text: `Last run ${when(a.lastRunAt, now)} ago`, tone: "muted" }
          : { text: a.kind === "manual" ? "Never run" : "Not run yet", tone: "muted" };
  return (
    <li className="flex items-center gap-3 rounded-lg px-2 py-2.5 hover:bg-accent/40">
      <button
        type="button"
        onClick={() => navigate({ name: "automations", item: a.id })}
        className="flex min-w-0 flex-1 cursor-pointer items-start gap-3 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
      >
        <Icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1">
          <span
            className={cn("block truncate text-sm", !a.on && "text-muted-foreground")}
          >
            {a.name}
          </span>
          <span className="block truncate text-xs text-muted-foreground">
            {a.when} · {a.does}
          </span>
        </span>
        <span
          className={cn(
            "shrink-0 text-xs tabular-nums",
            status.tone === "warning" ? "text-warning" : "text-muted-foreground",
          )}
        >
          {status.text}
        </span>
      </button>
      <Switch
        aria-label={`${a.name} ${a.on ? "on" : "off"}`}
        checked={a.on}
        onCheckedChange={(on) =>
          a.trigger
            ? setEnabled(
                changeConfig,
                node.nodeDid,
                "Trigger",
                a.trigger.config.trigger_id,
                on,
              )
            : a.task &&
              setEnabled(
                changeConfig,
                node.nodeDid,
                "Task",
                a.id.slice(MANUAL.length),
                on,
              )
        }
      />
    </li>
  );
}

/* a step below the kit's tabs, as navigation rather than headings */
const TAB = "text-[13px]";

function Count({ n }: { n: number }) {
  return <span className="text-xs text-muted-foreground tabular-nums">{n}</span>;
}
