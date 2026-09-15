/* Sessions: a heading row with search and New, then plain rows on the
   ground: title, the behaviour's chip, and when it last moved. */
import { useState } from "react";
import { MessageSquare, Plus, Search, X } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import { isLive } from "@/lib/live";
import { Input } from "@gents/ui/components/input";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import type { Shell } from "@/hooks/useShell";
import { href } from "@/lib/router";
import { BehaviorChip } from "./parts";
import { when } from "./time";
import { SessionStatus } from "./SessionStatus";

export function SessionsScreen({ shell }: { shell: Shell }) {
  const deployment = shell.selectedDeployment;
  const held = new Set(shell.holds.flatMap((h) => (h.sessionId ? [h.sessionId] : [])));
  const [query, setQuery] = useState<string | null>(null);
  /* the three axes the summary carries: behaviour, state, and what started it */
  const [behavior, setBehavior] = useState<string>("");
  const [state, setState] = useState<"" | "live" | "held" | "failed">("");
  const [source, setSource] = useState<"" | "person" | "task" | "trigger">("");
  const conversations = (deployment?.sessions ?? []).filter((c) => {
    if (query && !(c.title ?? "").toLowerCase().includes(query.toLowerCase()))
      return false;
    if (behavior && c.behaviorId !== behavior) return false;
    if (state === "live" && !isLive(c.turnState)) return false;
    if (state === "held" && !held.has(c.sessionId)) return false;
    if (state === "failed" && c.turnState !== "failed") return false;
    if (source === "task" && !c.taskId) return false;
    if (source === "trigger" && !c.triggerId) return false;
    if (source === "person" && (c.taskId || c.triggerId)) return false;
    return true;
  });
  const hasFilters = Boolean(behavior || state || source);
  const behaviorItems = [
    { value: "", label: "All behaviours" },
    ...(deployment?.behaviors ?? []).map((item) => ({
      value: item.behaviorId,
      label: item.displayName,
    })),
  ];
  const stateItems = [
    { value: "", label: "Any state" },
    { value: "live", label: "Live" },
    { value: "held", label: "Needs you" },
    { value: "failed", label: "Failed" },
  ];
  const sourceItems = [
    { value: "", label: "Any source" },
    { value: "person", label: "A person" },
    { value: "task", label: "A task" },
    { value: "trigger", label: "A trigger" },
  ];
  return (
    <ScrollArea className="h-full" data-testid="sessions-screen">
      <div className="mx-auto max-w-page px-6 py-6">
        <div className="flex h-10 items-center gap-2">
          <h1 className="font-heading text-lg font-medium text-heading">Sessions</h1>
          <div className="ml-auto flex items-center gap-2">
            {query === null ? (
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label="Search sessions"
                onClick={() => setQuery("")}
              >
                <Search />
              </Button>
            ) : (
              <div className="relative">
                <Input
                  autoFocus
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  onKeyDown={(e) => e.key === "Escape" && setQuery(null)}
                  placeholder="Search sessions"
                  className="h-8 w-56 pr-8"
                  aria-label="Search sessions"
                />
                <Button
                  variant="ghost"
                  size="icon-xs"
                  aria-label="Clear search"
                  className="absolute top-1 right-1 text-muted-foreground"
                  onClick={() => setQuery(null)}
                >
                  <X />
                </Button>
              </div>
            )}
            <Button
              variant="brand"
              size="sm"
              nativeButton={false}
              render={<a href={href({ name: "session", sessionId: null })} />}
            >
              New
            </Button>
          </div>
        </div>
        <div
          aria-label="Session filters"
          className="mt-3 flex flex-wrap items-end gap-2 rounded-lg border border-border/60 px-3 py-2.5"
        >
          <label className="grid gap-1 text-xs text-muted-foreground">
            Behaviour
            <Select
              items={behaviorItems}
              value={behavior}
              onValueChange={(value) => setBehavior(value ?? "")}
            >
              <SelectTrigger aria-label="Filter by behaviour" className="w-44">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {behaviorItems.map((item) => (
                  <SelectItem key={item.value || "all"} value={item.value}>
                    {item.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </label>
          <label className="grid gap-1 text-xs text-muted-foreground">
            State
            <Select
              items={stateItems}
              value={state}
              onValueChange={(value) => setState((value ?? "") as typeof state)}
            >
              <SelectTrigger aria-label="Filter by state" className="w-36">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {stateItems.map((item) => (
                  <SelectItem key={item.value || "any"} value={item.value}>
                    {item.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </label>
          <label className="grid gap-1 text-xs text-muted-foreground">
            Started by
            <Select
              items={sourceItems}
              value={source}
              onValueChange={(value) => setSource((value ?? "") as typeof source)}
            >
              <SelectTrigger aria-label="Filter by source" className="w-36">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {sourceItems.map((item) => (
                  <SelectItem key={item.value || "any"} value={item.value}>
                    {item.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </label>
          {hasFilters && (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                setBehavior("");
                setState("");
                setSource("");
              }}
            >
              <X /> Clear filters
            </Button>
          )}
          <span className="ml-auto pb-2 text-xs text-muted-foreground">
            Showing {conversations.length} of {deployment?.sessions.length ?? 0}
          </span>
        </div>
        <ul className="mt-4 divide-y divide-border/60">
          {conversations.map((c) => (
            <li key={c.sessionId}>
              <a
                href={href({ name: "session", sessionId: c.sessionId })}
                data-testid={`session-${c.sessionId}`}
                className="-mx-3 grid grid-cols-[auto_1fr_auto_auto] items-center gap-4 rounded-lg px-3 py-3.5 hover:bg-accent"
              >
                <SessionStatus turnState={c.turnState} held={held.has(c.sessionId)} />
                <p className="truncate text-sm">{c.title ?? "Untitled"}</p>
                <BehaviorChip
                  behaviorId={c.behaviorId}
                  deployment={deployment}
                  showName={false}
                  description={shell.behaviorDescriptions[c.behaviorId ?? ""]}
                />
                <span className="w-20 text-right text-sm text-muted-foreground">
                  {when(c.updatedAt)}
                </span>
              </a>
            </li>
          ))}
          {conversations.length === 0 && (query || hasFilters) && (
            <li className="py-8 text-center text-sm text-muted-foreground">
              No sessions match.
            </li>
          )}
          {conversations.length === 0 && !query && !hasFilters && (
            <li className="grid min-h-[50vh] place-items-center animate-in fade-in-0 slide-in-from-bottom-2 duration-300 ease-out fill-mode-both motion-reduce:animate-none">
              <div className="text-center">
                <MessageSquare className="mx-auto size-6 text-muted-foreground" />
                <p className="mt-3 font-heading text-lg font-medium text-heading">
                  No sessions yet
                </p>
                <Button
                  variant="brand"
                  className="mt-5"
                  nativeButton={false}
                  render={<a href={href({ name: "session", sessionId: null })} />}
                >
                  <Plus /> New session
                </Button>
              </div>
            </li>
          )}
        </ul>
      </div>
    </ScrollArea>
  );
}
