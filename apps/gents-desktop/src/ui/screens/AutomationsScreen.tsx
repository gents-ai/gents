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
import { browserStorage } from "@/lib/storage";
import { useSelectedNode } from "@/hooks/useClient";
import { Hint } from "./Hint";
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

/** What runs on its own on the active node, in one column: the
    automations, then templates to start a new one from. Opening one shows
    it in place of the list. */
export function AutomationsScreen({ item }: { item?: string }) {
  const node = useSelectedNode();
  const now = useMinute();
  const [filter, setFilter] = useState<AutomationTab>("all");
  const [query, setQuery] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [template, setTemplate] = useState<AutomationTemplate | null>(null);
  const [templatesHidden, hideTemplates] = useTemplatesHidden();
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
    <ScrollArea className="h-full" data-testid="automations-screen">
      <div className="mx-auto grid max-w-page gap-6 px-6 py-6">
        {open && node ? (
          <div className="grid gap-4">
            <a
              href={href({ name: "automations" })}
              className="inline-flex w-fit items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
            >
              <ArrowLeft className="size-3" /> Automations
            </a>
            <AutomationDetail node={node} automation={open} />
          </div>
        ) : (
          <>
            <header className="grid gap-3">
              <div className="flex flex-wrap items-start gap-x-4 gap-y-3">
                <div className="min-w-0 flex-1">
                  <h1 className="font-heading text-lg font-medium text-heading">
                    Automations
                  </h1>
                  <p className="mt-0.5 text-sm text-muted-foreground">
                    What runs on its own on {name}: on a schedule, when something
                    happens, or when you start it.
                  </p>
                </div>
                {all.length > 0 && (
                  <div className="flex items-center gap-1">
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
                      className={cn(
                        "text-muted-foreground",
                        query !== null && "text-foreground",
                      )}
                      onClick={() => setQuery(query === null ? "" : null)}
                    >
                      <Search />
                    </Button>
                    {node && (
                      <Button size="sm" variant="secondary" onClick={() => start(null)}>
                        <Plus /> New automation
                      </Button>
                    )}
                  </div>
                )}
              </div>
              {query !== null && (
                <div className="relative">
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
                    className="absolute top-1/2 right-1 -translate-y-1/2 text-muted-foreground"
                    onClick={() => setQuery(null)}
                  >
                    <X />
                  </Button>
                </div>
              )}
              {filter !== "all" && (
                <button
                  type="button"
                  onClick={() => setFilter("all")}
                  className="flex w-fit cursor-pointer items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
                >
                  Showing {FILTERS.find((f) => f.key === filter)!.label.toLowerCase()}
                  <X className="size-3" />
                </button>
              )}
            </header>
            {node && all.length > 0 && !templatesHidden && (
              <TemplateRow onUse={start} onHide={() => hideTemplates(true)} />
            )}
            {all.length === 0 ? (
              <>
                <Empty node={name} onNew={node ? () => start(null) : undefined} />
                {node && <Templates onUse={start} />}
              </>
            ) : shown.length === 0 ? (
              <p className="text-sm text-muted-foreground">Nothing matches.</p>
            ) : (
              <>
                {scheduled.length > 0 && <List automations={scheduled} now={now} />}
                {byHand.length > 0 && (
                  <section className="grid gap-2">
                    <h2 className="font-mono text-[11px] tracking-wide text-muted-foreground uppercase">
                      Run by hand
                    </h2>
                    <List automations={byHand} now={now} />
                  </section>
                )}
              </>
            )}
            {node && all.length > 0 && templatesHidden && (
              <button
                type="button"
                onClick={() => hideTemplates(false)}
                className="w-fit cursor-pointer text-xs text-muted-foreground hover:text-foreground"
              >
                Show templates
              </button>
            )}
          </>
        )}
      </div>
      {node && (
        <NewAutomationDialog
          deployment={node}
          open={creating}
          onOpenChange={setCreating}
          template={template}
          onCreated={(triggerId) => navigate({ name: "automations", item: triggerId })}
        />
      )}
    </ScrollArea>
  );
}

const ICON = { schedule: Timer, event: Zap, manual: Hand };

/* what a row says beside its name: why it can't run, how its last run
   went, or when it runs next */
function statusOf(a: Automation, now: number): { text: string; warn: boolean } {
  if (!a.on) return { text: "Off", warn: false };
  if (!a.readiness.ok) return { text: a.readiness.reason, warn: true };
  if (a.lastFailed) return { text: `Failed ${when(a.lastRunAt, now)} ago`, warn: true };
  const next = a.nextRunAt ? Date.parse(a.nextRunAt) : null;
  if (next && next > now) return { text: `Next in ${span(next - now)}`, warn: false };
  if (a.lastRunAt) return { text: `Ran ${when(a.lastRunAt, now)} ago`, warn: false };
  return { text: "Not run yet", warn: false };
}

function List({ automations, now }: { automations: Automation[]; now: number }) {
  return (
    <ul className="divide-y divide-border/60">
      {automations.map((a) => (
        <li key={a.id}>
          <Row automation={a} now={now} />
        </li>
      ))}
    </ul>
  );
}

/* drawn like a sessions row: a highlight that reaches past the column,
   the name and a muted line under it, the status at the end */
function Row({ automation: a, now }: { automation: Automation; now: number }) {
  const Icon = ICON[a.kind];
  const status = statusOf(a, now);
  return (
    <a
      href={href({ name: "automations", item: a.id })}
      className="-mx-3 grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-4 rounded-lg px-3 py-3.5 transition-colors outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring"
    >
      <Icon className="size-4 text-muted-foreground" />
      <span className="min-w-0">
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
          "max-w-48 truncate text-right text-sm",
          status.warn ? "text-warning" : "text-muted-foreground",
        )}
      >
        {status.text}
      </span>
    </a>
  );
}

/* with no automations yet: what one is, before the templates */
function Empty({ node, onNew }: { node: string; onNew?: () => void }) {
  return (
    <section
      aria-label="No automations yet"
      className="grid justify-items-center gap-2 rounded-xl border border-dashed border-border/60 px-6 py-10 text-center"
    >
      <span className="grid size-12 place-items-center rounded-2xl bg-raised text-muted-foreground">
        <Play className="size-6" />
      </span>
      <h2 className="mt-2 font-heading text-lg font-medium text-heading">
        Automate work on {node}
      </h2>
      <p className="max-w-md text-sm text-muted-foreground">
        An agent can run a task for you on a schedule, when something happens, or
        whenever you start it. Start from a template below, or from scratch.
      </p>
      {onNew && (
        <Button size="sm" variant="secondary" className="mt-2" onClick={onNew}>
          <Plus /> New automation
        </Button>
      )}
    </section>
  );
}

const HIDE_TEMPLATES = "gents.automations.hideTemplates";
const SHOWN = 4;
/* the node summary cards' surface */
const SURFACE =
  "cursor-pointer rounded-xl bg-raised/50 text-left transition-colors outline-none hover:bg-raised focus-visible:bg-raised";

/** Whether the templates row is hidden on this device. */
function useTemplatesHidden() {
  const [hidden, setHidden] = useState(
    () => browserStorage.getItem(HIDE_TEMPLATES) === "1",
  );
  const hide = (next: boolean) => {
    setHidden(next);
    if (next) browserStorage.setItem(HIDE_TEMPLATES, "1");
    else browserStorage.removeItem(HIDE_TEMPLATES);
  };
  return [hidden, hide] as const;
}

/* ready-made automations, each opening New automation filled in: a short
   row of cards under the heading, with the rest a click away */
function TemplateRow({
  onUse,
  onHide,
}: {
  onUse: (t: AutomationTemplate) => void;
  onHide: () => void;
}) {
  const [more, setMore] = useState(false);
  return (
    <section aria-label="Templates" className="grid gap-2">
      <div className="flex items-center gap-2">
        <h2 className="flex-1 text-xs text-muted-foreground">Start from a template</h2>
        {TEMPLATES.length > SHOWN && (
          <button
            type="button"
            onClick={() => setMore(!more)}
            className="cursor-pointer text-xs text-muted-foreground hover:text-foreground"
          >
            {more ? "Fewer" : `${TEMPLATES.length - SHOWN} more`}
          </button>
        )}
        <Button
          variant="ghost"
          size="icon-xs"
          aria-label="Hide templates"
          className="text-muted-foreground"
          onClick={onHide}
        >
          <X />
        </Button>
      </div>
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
        {(more ? TEMPLATES : TEMPLATES.slice(0, SHOWN)).map((t) => (
          <Hint key={t.id} label={t.blurb}>
            <button
              type="button"
              onClick={() => onUse(t)}
              className={cn(SURFACE, "flex min-w-0 items-center gap-2 px-3 py-2.5")}
            >
              <t.icon className="size-4 shrink-0 text-muted-foreground" />
              <span className="truncate text-sm">{t.name}</span>
            </button>
          </Hint>
        ))}
      </div>
    </section>
  );
}

/* with no automations yet: every template, with what it does */
function Templates({ onUse }: { onUse: (t: AutomationTemplate) => void }) {
  return (
    <section aria-labelledby="automation-templates" className="grid gap-3">
      <h2
        id="automation-templates"
        className="font-mono text-[11px] tracking-wide text-muted-foreground uppercase"
      >
        Start from a template
      </h2>
      <div className="grid gap-3 sm:grid-cols-2" aria-label="Templates">
        {TEMPLATES.map((t) => (
          <button
            key={t.id}
            type="button"
            onClick={() => onUse(t)}
            className={cn(SURFACE, "flex items-start gap-3 px-4 py-3.5")}
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
