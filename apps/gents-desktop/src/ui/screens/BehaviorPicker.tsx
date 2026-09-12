/* The behaviour choice for a new session: a chip, the name and a chevron
   in the composer's leading slot, opening a small picker. Each row is the
   behaviour's chip and name, its description, and one mono line of what
   it runs on and may touch, resolved by the bridge; a search field
   filters by name and description. Disabled behaviours are left out. */
import { useEffect, useRef, useState } from "react";
import { ChevronDown, Plus, Search, X } from "lucide-react";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Popover, PopoverContent, PopoverTrigger } from "@gents/ui/components/popover";
import { cn } from "@gents/ui/lib/utils";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import type { Shell } from "@/hooks/useShell";
import { createBehavior } from "./agent/createBehavior";
import { BehaviorAvatar } from "./parts";
import { behaviorReadiness } from "@/lib/send-status";

/* the access modes at a glance, short enough for one line */
const short = (mode: string | null | undefined) =>
  ({ "read / write": "rw", "read-only": "ro", unrestricted: "any", off: "off" })[
    mode ?? "off"
  ] ?? mode;

export function BehaviorPicker({
  shell,
  deployment,
  behaviorId,
  onChange,
}: {
  shell: Shell;
  deployment: DeploymentView | null;
  behaviorId: string | null;
  onChange: (behaviorId: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  /* search is hidden until asked for: the icon in the footer, or typing */
  const [searching, setSearching] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (searching) input.current?.focus();
  }, [searching]);
  const behaviours = deployment?.behaviors.filter((b) => b.enabled) ?? [];
  const chosen = behaviours.find((b) => b.behaviorId === behaviorId) ?? behaviours[0];
  if (!chosen) return null;
  const describe = (id: string) => shell.behaviorDescriptions[id] ?? "";
  const readiness = (id: string) => behaviorReadiness(deployment, id);
  const env = (id: string) =>
    deployment?.behaviorEnvironments.find((e) => e.behaviorId === id);
  const q = query.trim().toLowerCase();
  const shown = behaviours.filter(
    (b) =>
      !q ||
      b.displayName.toLowerCase().includes(q) ||
      describe(b.behaviorId).toLowerCase().includes(q),
  );
  return (
    <Popover
      open={open}
      onOpenChange={(o) => {
        setOpen(o);
        if (!o) {
          setQuery("");
          setSearching(false);
        }
      }}
    >
      <PopoverTrigger
        render={
          <Button variant="ghost" size="sm" className="gap-2 px-1.5 font-normal" />
        }
        aria-label="Behaviour"
      >
        <BehaviorAvatar
          name={chosen.displayName}
          behaviorId={chosen.behaviorId}
          className="size-6 text-[10px]"
        />
        <span>{chosen.displayName}</span>
        <ChevronDown className="ml-6 size-3.5 text-muted-foreground" />
      </PopoverTrigger>
      <PopoverContent
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
              placeholder="Search behaviours"
              aria-label="Search behaviours"
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
          <ul role="listbox" aria-label="Behaviour" className="p-1">
            {shown.map((b) => {
              const e = env(b.behaviorId);
              const selected = b.behaviorId === chosen.behaviorId;
              return (
                <li key={b.behaviorId}>
                  <button
                    type="button"
                    role="option"
                    aria-selected={selected}
                    onClick={() => {
                      onChange(b.behaviorId);
                      setOpen(false);
                    }}
                    className={cn(
                      "grid w-full grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 rounded-lg px-2 py-2 text-left hover:bg-accent",
                      selected && "bg-muted",
                    )}
                  >
                    <BehaviorAvatar
                      name={b.displayName}
                      behaviorId={b.behaviorId}
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
                    {describe(b.behaviorId) && (
                      <span className="line-clamp-1 text-xs text-muted-foreground">
                        {describe(b.behaviorId)}
                      </span>
                    )}
                    {readiness(b.behaviorId).ready ? (
                      <span className="truncate font-mono text-[11px] text-muted-foreground">
                        {e?.modelName ?? "no backend"} · files {short(e?.fileAccess)} ·
                        bash {short(e?.bashAccess)} · net{" "}
                        {e?.networkAccess ?? "disabled"}
                      </span>
                    ) : (
                      <span className="truncate text-[11px] text-destructive">
                        Unavailable: {readiness(b.behaviorId).reason}
                      </span>
                    )}
                  </button>
                </li>
              );
            })}
            {shown.length === 0 && (
              <li className="px-2 py-4 text-center text-sm text-muted-foreground">
                No behaviour matches.
              </li>
            )}
          </ul>
        </ScrollArea>
        <div className="flex items-center border-t border-border/60 p-1">
          <button
            type="button"
            onClick={() => deployment && void createBehavior(shell, deployment)}
            className="flex min-w-0 flex-1 items-center gap-2 rounded-lg px-2 py-2 text-left text-sm hover:bg-accent"
          >
            <span className="grid size-7 place-items-center rounded-full border border-dashed border-border text-muted-foreground">
              <Plus className="size-3.5" />
            </span>
            Create new behaviour
          </button>
          {!searching && (
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="Search behaviours"
              onClick={() => setSearching(true)}
            >
              <Search />
            </Button>
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}
