/* The agent choice for a new session: a chip, the name and a chevron
   in the composer's leading slot, opening a small picker. Each row is the
   agent's chip and name, its description, and one mono line of what
   it runs on and may touch, resolved by the bridge; a search field
   filters by name and description. Disabled agents are left out. */
import { useEffect, useRef, useState } from "react";
import { ChevronDown, Plus, Search, X } from "lucide-react";
import type { NodeView } from "../../hooks/fleetStore";
import { Button } from "@gents/ui/components/button";
import { Popover, PopoverContent, PopoverTrigger } from "@gents/ui/components/popover";
import { cn } from "@gents/ui/lib/utils";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { AgentSheet } from "./agent/BehaviorSheet";
import { shortAccess } from "./behavior";
import { AgentInitials } from "./parts";
import { agentReadiness } from "@/lib/agent-readiness";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";

export function AgentPicker({
  deployment,
  agentId,
  onChange,
}: {
  deployment: NodeView | null;
  agentId: string | null;
  onChange: (agentId: string) => void;
}) {
  if (!deployment) return null;
  const agents = deployment.agents.filter((b) => b.enabled);
  const chosen = agents.find((b) => b.agentId === agentId) ?? agents[0];
  if (!chosen) return null;
  return (
    <MountedAgentPicker
      deployment={deployment}
      agents={agents}
      chosen={chosen}
      onChange={onChange}
    />
  );
}

function MountedAgentPicker({
  deployment,
  agents,
  chosen,
  onChange,
}: {
  deployment: NodeView;
  agents: NodeView["agents"];
  chosen: NodeView["agents"][number];
  onChange: (agentId: string) => void;
}) {
  const [query, setQuery] = useState("");
  /* search is hidden until asked for: the icon in the footer, or typing */
  const [searching, setSearching] = useState(false);
  const [creating, setCreating] = useState(false);
  /* the create sheet waits for the popover's exit animation: one dialog at a time */
  const createAfterClose = useRef(false);
  const popover = useExclusivePopover(() => {
    setQuery("");
    setSearching(false);
  });
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (searching) input.current?.focus();
  }, [searching]);
  /* described by the node whose agents are listed */
  const describe = (id: string) =>
    deployment.agents.find((b) => b.agentId === id)?.description ?? "";
  const readiness = (id: string) => agentReadiness(deployment, id);
  const env = (id: string) =>
    deployment.agentEnvironments.find((e) => e.agentId === id);
  const q = query.trim().toLowerCase();
  const shown = agents.filter(
    (b) =>
      !q ||
      b.displayName.toLowerCase().includes(q) ||
      describe(b.agentId).toLowerCase().includes(q),
  );
  return (
    <>
      <Popover
        open={popover.open}
        onOpenChange={(next) => {
          /* reopening the picker withdraws a create still waiting on its exit */
          if (next) createAfterClose.current = false;
          popover.onOpenChange(next);
        }}
        onOpenChangeComplete={(next) => {
          /* a popover opened after Create was clicked takes the turn instead */
          const create = !next && createAfterClose.current && !popover.peerPending();
          if (!next) createAfterClose.current = false;
          popover.onOpenChangeComplete(next);
          if (create) setCreating(true);
        }}
      >
        <PopoverTrigger
          render={
            <Button variant="ghost" size="sm" className="gap-2 px-1.5 font-normal" />
          }
          aria-label="Agent"
        >
          <AgentInitials name={chosen.displayName} className="size-6 text-[10px]" />
          <span>{chosen.displayName}</span>
          <ChevronDown className="ml-6 size-3.5 text-muted-foreground" />
        </PopoverTrigger>
        <PopoverContent
          ref={popover.popupRef}
          aria-label="Choose agent"
          align="start"
          className="w-[min(40rem,calc(100vw-4rem))] p-0"
          onKeyDown={(e) => {
            /* a printable key with the list focused starts a search with that key */
            if (
              !searching &&
              e.key.length === 1 &&
              !e.metaKey &&
              !e.ctrlKey &&
              !e.altKey
            ) {
              setSearching(true);
              setQuery(e.key);
              e.preventDefault();
            }
          }}
        >
          {searching && (
            <div className="flex items-center gap-2 border-b border-border/60 px-3">
              <Search className="size-3.5 text-muted-foreground" />
              <input
                ref={input}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Escape") {
                    e.stopPropagation();
                    setQuery("");
                    setSearching(false);
                  }
                }}
                placeholder="Search agents"
                aria-label="Search agents"
                className="h-9 w-full bg-transparent text-sm outline-none placeholder:text-muted-foreground"
              />
              <button
                type="button"
                aria-label="Close search"
                className="text-muted-foreground hover:text-foreground"
                onClick={() => {
                  setQuery("");
                  setSearching(false);
                }}
              >
                <X className="size-3.5" />
              </button>
            </div>
          )}
          <ScrollArea className="max-h-[min(20rem,calc(var(--available-height)-8rem))] [&_[data-slot=scroll-area-viewport]]:max-h-[inherit]">
            <ul role="listbox" aria-label="Agent" className="p-1">
              {shown.map((b) => {
                const e = env(b.agentId);
                const selected = b.agentId === chosen.agentId;
                return (
                  <li key={b.agentId}>
                    <button
                      type="button"
                      role="option"
                      aria-selected={selected}
                      onClick={() => {
                        onChange(b.agentId);
                        popover.onOpenChange(false);
                      }}
                      className={cn(
                        "grid w-full grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 rounded-lg px-2 py-2 text-left hover:bg-accent",
                        selected && "bg-muted",
                      )}
                    >
                      <AgentInitials
                        name={b.displayName}
                        className="row-span-3 mt-0.5"
                      />
                      <span className="text-sm font-medium">
                        {b.displayName}
                        {b.isDefault && (
                          <span className="ml-2 font-normal text-muted-foreground">
                            default
                          </span>
                        )}
                      </span>
                      {describe(b.agentId) && (
                        <span className="line-clamp-1 text-xs text-muted-foreground">
                          {describe(b.agentId)}
                        </span>
                      )}
                      {readiness(b.agentId).ready ? (
                        <span className="truncate font-mono text-[11px] text-muted-foreground">
                          {e?.modelName ?? "no backend"} · files{" "}
                          {shortAccess(e?.fileAccess)} · bash{" "}
                          {shortAccess(e?.bashAccess)} · net{" "}
                          {e?.networkAccess ?? "disabled"}
                        </span>
                      ) : (
                        <span className="truncate text-[11px] text-destructive">
                          Unavailable: {readiness(b.agentId).reason}
                        </span>
                      )}
                    </button>
                  </li>
                );
              })}
              {shown.length === 0 && (
                <li className="px-2 py-4 text-center text-sm text-muted-foreground">
                  No agent matches.
                </li>
              )}
            </ul>
          </ScrollArea>
          <div className="flex items-center border-t border-border/60 p-1">
            <button
              type="button"
              onClick={() => {
                /* a draft agent beside the session; saved enabled and chosen */
                createAfterClose.current = true;
                popover.onOpenChange(false);
              }}
              className="flex min-w-0 flex-1 items-center gap-2 rounded-lg px-2 py-2 text-left text-sm hover:bg-accent"
            >
              <span className="grid size-7 place-items-center rounded-full border border-dashed border-border text-muted-foreground">
                <Plus className="size-3.5" />
              </span>
              Create new agent
            </button>
            {!searching && (
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label="Search agents"
                onClick={() => setSearching(true)}
              >
                <Search />
              </Button>
            )}
          </div>
        </PopoverContent>
      </Popover>
      {deployment && (
        <AgentSheet
          deployment={deployment}
          open={creating}
          enabled
          onClose={(agentId) => {
            setCreating(false);
            if (agentId) onChange(agentId);
          }}
        />
      )}
    </>
  );
}
