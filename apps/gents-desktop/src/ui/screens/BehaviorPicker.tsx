import { useEffect, useRef, useState } from "react";
import { ChevronDown, Package, Plus, Search } from "lucide-react";
import type { NodeView } from "../../hooks/fleetStore";
import { Button } from "@gents/ui/components/button";
import { Popover, PopoverContent, PopoverTrigger } from "@gents/ui/components/popover";
import { cn } from "@gents/ui/lib/utils";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { AgentSheet } from "./agent/BehaviorSheet";
import { packOrigin, usePackGroups } from "./agent/ListDetail";
import { shortAccess } from "./behavior";
import { AgentInitials } from "./parts";
import { agentReadiness } from "@/lib/agent-readiness";
import { useNodeConfigReadOnly } from "@/hooks/useClient";
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
  const readOnly = useNodeConfigReadOnly(deployment);
  const [query, setQuery] = useState("");
  const [creating, setCreating] = useState(false);
  // Wait for the popover's exit animation so two dialogs are never open at once.
  const createAfterClose = useRef(false);
  const popover = useExclusivePopover(() => setQuery(""));
  const input = useRef<HTMLInputElement>(null);
  /* the field takes what the person types from the moment the list opens */
  useEffect(() => {
    if (popover.open) input.current?.focus();
  }, [popover.open]);
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
  /* the person's own agents first, the default leading; a pack's agents,
     often stages of its work rather than ones to talk to, under its name,
     folded unless none of the person's own show, a search is on, or the
     chosen agent is among them */
  const own = shown
    .filter((b) => packOrigin(b.tags) === null)
    .sort((a, b) => Number(b.isDefault) - Number(a.isDefault));
  const packs = Array.from(
    new Set(
      shown.map((b) => packOrigin(b.tags)).filter((o): o is string => o !== null),
    ),
  ).sort();
  const packGroups = usePackGroups("agent-picker");
  const openByDefault = own.length === 0;
  const row = (b: NodeView["agents"][number]) => {
    const e = env(b.agentId);
    const selected = b.agentId === chosen.agentId;
    const ready = readiness(b.agentId);
    return (
      <li key={b.agentId}>
        <button
          type="button"
          role="option"
          aria-selected={selected}
          title={
            ready.ready
              ? `${e?.modelName ?? "no backend"} · files ${shortAccess(e?.fileAccess)} · bash ${shortAccess(e?.bashAccess)} · net ${e?.networkAccess ?? "disabled"}`
              : `Unavailable: ${ready.reason}`
          }
          onClick={() => {
            onChange(b.agentId);
            popover.onOpenChange(false);
          }}
          className={cn(
            "flex h-7 w-full min-w-0 items-center gap-2 rounded-md px-2 text-left text-xs hover:bg-accent",
            selected && "bg-muted",
          )}
        >
          <AgentInitials name={b.displayName} className="size-5 shrink-0 text-[9px]" />
          <span className="shrink-0 font-medium">{b.displayName}</span>
          {b.isDefault && (
            <span className="shrink-0 text-xs text-muted-foreground">default</span>
          )}
          {ready.ready ? (
            describe(b.agentId) && (
              <span className="min-w-0 truncate text-xs text-muted-foreground">
                {describe(b.agentId)}
              </span>
            )
          ) : (
            <span className="min-w-0 truncate text-xs text-destructive">
              Unavailable
            </span>
          )}
        </button>
      </li>
    );
  };
  return (
    <>
      <Popover
        open={popover.open}
        onOpenChange={(next) => {
          if (next) createAfterClose.current = false;
          popover.onOpenChange(next);
        }}
        onOpenChangeComplete={(next) => {
          const create = !next && createAfterClose.current && !popover.peerPending();
          if (!next) createAfterClose.current = false;
          popover.onOpenChangeComplete(next);
          if (create) setCreating(true);
        }}
      >
        <PopoverTrigger
          render={
            /* the same size as the folder chip beside it under the composer */
            <Button
              variant="ghost"
              size="sm"
              className="h-7 gap-1.5 px-1.5 text-xs font-normal"
            />
          }
          aria-label="Agent"
        >
          <AgentInitials name={chosen.displayName} className="size-5 text-[9px]" />
          <span>{chosen.displayName}</span>
          <ChevronDown className="size-3 text-muted-foreground" />
        </PopoverTrigger>
        <PopoverContent
          ref={popover.popupRef}
          aria-label="Choose agent"
          align="start"
          className="w-[min(28rem,calc(100vw-4rem))] gap-0 p-0"
        >
          <div className="flex items-center gap-2 border-b border-border/60 py-1 pr-1 pl-3">
            <Search aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
            <input
              ref={input}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                /* the first Escape clears a search; the next closes the list */
                if (e.key === "Escape" && query) {
                  e.stopPropagation();
                  setQuery("");
                }
              }}
              placeholder="Search agents"
              aria-label="Search agents"
              className="h-8 min-w-0 flex-1 bg-transparent text-xs outline-none placeholder:text-muted-foreground"
            />
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="Create new agent"
              title={readOnly ?? "Create new agent"}
              disabled={Boolean(readOnly)}
              onClick={() => {
                createAfterClose.current = true;
                popover.onOpenChange(false);
              }}
            >
              <Plus />
            </Button>
          </div>
          <ScrollArea className="max-h-[min(20rem,calc(var(--available-height)-8rem))] [&_[data-slot=scroll-area-viewport]]:max-h-[inherit]">
            <ul role="listbox" aria-label="Agent" className="p-1">
              {own.map(row)}
              {packs.map((pack) => {
                const members = shown.filter((b) => packOrigin(b.tags) === pack);
                const open =
                  Boolean(q) ||
                  members.some((b) => b.agentId === chosen.agentId) ||
                  packGroups.isOpen(pack, openByDefault);
                return (
                  <li key={`pack:${pack}`} role="group" aria-label={`From ${pack}`}>
                    <button
                      type="button"
                      aria-expanded={open}
                      onClick={() => packGroups.toggle(pack, openByDefault)}
                      className="flex h-7 w-full min-w-0 items-center gap-1.5 rounded-md px-2 text-left text-xs text-muted-foreground hover:text-foreground"
                    >
                      <ChevronDown
                        aria-hidden
                        className={cn(
                          "size-3.5 shrink-0 transition-transform",
                          !open && "-rotate-90",
                        )}
                      />
                      <Package aria-hidden className="size-3.5 shrink-0" />
                      <span className="truncate">{pack}</span>
                      <span className="shrink-0">· {members.length}</span>
                    </button>
                    {open && <ul className="grid">{members.map(row)}</ul>}
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
        </PopoverContent>
      </Popover>
      {!readOnly && (
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
