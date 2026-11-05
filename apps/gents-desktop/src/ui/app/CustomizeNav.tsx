import { useRef, useState, type ReactElement, type ReactNode } from "react";
import { ChevronDown, GripVertical, Minus, Plus, X } from "lucide-react";
import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";
import { Button } from "@gents/ui/components/button";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSub,
  ContextMenuSubContent,
  ContextMenuSubTrigger,
  ContextMenuTrigger,
} from "@gents/ui/components/context-menu";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import { cn } from "@gents/ui/lib/utils";
import { ContribBoundary } from "@/contrib/react/boundary";
import { SidebarModeItems } from "./sidebarModes";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import {
  preferences,
  LISTED_COUNTS,
  RECENT_COUNTS,
  useFlag,
  useNavLayout,
} from "@/preferences";
import {
  arrange,
  DEFAULT_RECENT,
  NAV_STATUS,
  DEFAULT_LISTED,
  isDivider,
  mailboxGroupOf,
  newDividerId,
  type EntryGroup,
  useNavCandidates,
  useNavFooterCandidates,
  useNavSectionCandidates,
} from "./navLayout";

const sheet = createStore<{ open: boolean }>(() => ({ open: false }));

export const openCustomizeNav = () => sheet.setState({ open: true });

/** the sidebar, with how it shows on its context menu, and "Customize…"
    while customizing is on */
export function NavMenu({
  render,
  children,
}: {
  render: ReactElement;
  children: ReactNode;
}) {
  const customizable = useFlag("customize-nav");
  return (
    <ContextMenu>
      <ContextMenuTrigger render={render}>{children}</ContextMenuTrigger>
      <ContextMenuContent className="min-w-48">
        <ContextMenuSub>
          <ContextMenuSubTrigger>Sidebar</ContextMenuSubTrigger>
          <ContextMenuSubContent className="min-w-44">
            <SidebarModeItems />
          </ContextMenuSubContent>
        </ContextMenuSub>
        {customizable && (
          <ContextMenuItem onClick={openCustomizeNav}>Customize…</ContextMenuItem>
        )}
      </ContextMenuContent>
    </ContextMenu>
  );
}

const ROW = 30;

type Entry = {
  id: string;
  label: string;
  icon: ReactNode;
  own?: boolean;
  group?: EntryGroup;
};

/* an entry's name, and, faint after it, what it belongs to */
function EntryName({ entry }: { entry: Entry }) {
  return (
    <span className="min-w-0 flex-1 truncate">
      {entry.label}
      {entry.group && (
        <span className="text-muted-foreground"> · {entry.group.label}</span>
      )}
    </span>
  );
}

function EntryIcon({ entry }: { entry: Entry }) {
  return (
    <span className="grid size-5 shrink-0 place-items-center">
      {entry.own ? (
        entry.icon
      ) : (
        <ContribBoundary id={entry.id} variant="chip">
          {entry.icon}
        </ContribBoundary>
      )}
    </span>
  );
}

function RemoveButton({ entry, onRemove }: { entry: Entry; onRemove: () => void }) {
  return (
    <button
      type="button"
      aria-label={`Remove ${entry.label}`}
      onClick={onRemove}
      className="grid size-6 shrink-0 place-items-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
    >
      <X className="size-3.5" />
    </button>
  );
}

const moved = (ids: string[], from: number, to: number) => {
  const next = [...ids];
  const [id] = next.splice(from, 1);
  next.splice(to, 0, id!);
  return next;
};

/* one group of the sheet, listing what the nav shows: drag a grip, or focus
   it and press ↑ or ↓, to move an entry; remove one with its ×, and add a
   removed one back, at the group's end, from Add */
function NavGroup({
  title,
  lead = [],
  entries,
  onChange,
  extra,
  newDivider,
}: {
  title: string;
  /** kept in their place before the others; they can only be removed */
  lead?: { entry: Entry; shown: boolean }[];
  /** every entry of the group in order, each with whether it is shown */
  entries: { entry: Entry; shown: boolean }[];
  /** the group's new order, and the entries whose shown state changes */
  onChange: (order: string[], shown: Record<string, boolean>) => void;
  /** drawn at the end of an entry's row */
  extra?: (entry: Entry) => ReactNode;
  /** the group takes dividers: Add always offers one, under this new id */
  newDivider?: () => string;
}) {
  const [drag, setDrag] = useState<{ from: number; dy: number } | null>(null);
  const startY = useRef(0);
  const list = useRef<HTMLUListElement>(null);
  const visible = entries.filter(({ shown }) => shown).map(({ entry }) => entry);
  const hiddenIds = entries.filter(({ shown }) => !shown).map(({ entry }) => entry.id);
  const visibleIds = visible.map((entry) => entry.id);
  const addable = [...lead, ...entries]
    .filter(({ shown }) => !shown)
    .map(({ entry }) => entry);
  const move = (from: number, to: number) =>
    onChange([...moved(visibleIds, from, to), ...hiddenIds], {});
  const remove = (id: string) =>
    onChange([...visibleIds.filter((v) => v !== id), id, ...hiddenIds], {
      [id]: false,
    });
  const add = (id: string) =>
    onChange(
      lead.some(({ entry }) => entry.id === id)
        ? [...visibleIds, ...hiddenIds]
        : [...visibleIds, id, ...hiddenIds.filter((h) => h !== id)],
      { [id]: true },
    );
  const target = drag
    ? Math.max(0, Math.min(visible.length - 1, drag.from + Math.round(drag.dy / ROW)))
    : null;
  const offset = (i: number) => {
    if (!drag || target === null) return 0;
    if (i === drag.from) return drag.dy;
    if (drag.from < target && i > drag.from && i <= target) return -ROW;
    if (drag.from > target && i < drag.from && i >= target) return ROW;
    return 0;
  };
  return (
    <div className="grid gap-1">
      <p className="m-0 px-1 font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
        {title}
      </p>
      <ul ref={list} className="relative grid select-none" aria-label={title}>
        {lead
          .filter(({ shown }) => shown)
          .map(({ entry }) => (
            <li
              key={entry.id}
              style={{ height: ROW }}
              className="flex items-center gap-2 rounded-lg px-1 text-sm"
            >
              <span className="size-6 shrink-0" aria-hidden="true" />
              <EntryIcon entry={entry} />
              <EntryName entry={entry} />
              <RemoveButton
                entry={entry}
                onRemove={() =>
                  onChange([...visibleIds, ...hiddenIds], { [entry.id]: false })
                }
              />
            </li>
          ))}
        {visible.map((entry, i) => (
          <li
            key={entry.id}
            style={{ height: ROW, transform: `translateY(${offset(i)}px)` }}
            className={cn(
              "flex items-center gap-2 rounded-lg px-1 text-sm",
              drag?.from === i
                ? "relative z-10 bg-raised shadow-md"
                : drag && "transition-transform duration-150 ease-out",
            )}
          >
            <button
              type="button"
              aria-label={`Move ${entry.label}`}
              aria-keyshortcuts="ArrowUp ArrowDown"
              className="grid size-6 shrink-0 cursor-grab touch-none place-items-center rounded-md text-muted-foreground hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring active:cursor-grabbing"
              onKeyDown={(e) => {
                const to =
                  e.key === "ArrowUp" ? i - 1 : e.key === "ArrowDown" ? i + 1 : null;
                if (to === null || to < 0 || to >= visible.length) return;
                e.preventDefault();
                move(i, to);
                /* focus follows the entry to its new place */
                requestAnimationFrame(() =>
                  list.current
                    ?.querySelectorAll<HTMLButtonElement>("button[aria-label^='Move']")
                    [to]?.focus(),
                );
              }}
              onPointerDown={(e) => {
                if (e.button !== 0) return;
                e.currentTarget.setPointerCapture(e.pointerId);
                startY.current = e.clientY;
                setDrag({ from: i, dy: 0 });
              }}
              onPointerMove={(e) =>
                drag && setDrag({ from: drag.from, dy: e.clientY - startY.current })
              }
              onPointerUp={() => {
                if (drag && target !== null && target !== drag.from)
                  move(drag.from, target);
                setDrag(null);
              }}
              onPointerCancel={() => setDrag(null)}
            >
              <GripVertical className="size-4" />
            </button>
            <EntryIcon entry={entry} />
            <EntryName entry={entry} />
            {extra?.(entry)}
            <RemoveButton entry={entry} onRemove={() => remove(entry.id)} />
          </li>
        ))}
        {(addable.length > 0 || newDivider) && (
          <li style={{ height: ROW }} className="flex items-center px-1">
            <DropdownMenu>
              <DropdownMenuTrigger
                render={
                  <button
                    type="button"
                    aria-label={`Add to ${title}`}
                    className="flex h-6 items-center gap-2 rounded-md pr-2 text-sm text-muted-foreground transition-colors hover:text-foreground aria-expanded:text-foreground"
                  />
                }
              >
                <span className="grid size-6 place-items-center">
                  <Plus className="size-4" />
                </span>
                Add
              </DropdownMenuTrigger>
              <DropdownMenuContent align="start" className="min-w-44">
                {/* entries of a family under one submenu, where the first
                    of them would be */}
                {addable.map((entry, i) => {
                  const item = (e: Entry) => (
                    <DropdownMenuItem key={e.id} onClick={() => add(e.id)}>
                      <EntryIcon entry={e} />
                      {e.label}
                    </DropdownMenuItem>
                  );
                  const group = entry.group;
                  if (!group) return item(entry);
                  if (addable.findIndex((e) => e.group?.id === group.id) !== i)
                    return null;
                  return (
                    <DropdownMenuSub key={group.id}>
                      <DropdownMenuSubTrigger>
                        <span className="grid size-5 shrink-0 place-items-center">
                          {group.icon}
                        </span>
                        {group.label}
                      </DropdownMenuSubTrigger>
                      <DropdownMenuSubContent className="min-w-44">
                        {addable
                          .filter((e) => e.group?.id === group.id)
                          .flatMap((e, j, all) =>
                            /* everything, set apart from its parts */
                            e.id === "mailbox-all" && j < all.length - 1
                              ? [item(e), <DropdownMenuSeparator key="all-parts" />]
                              : [item(e)],
                          )}
                      </DropdownMenuSubContent>
                    </DropdownMenuSub>
                  );
                })}
                {newDivider && (
                  <>
                    {addable.length > 0 && <DropdownMenuSeparator />}
                    <DropdownMenuItem
                      onClick={() => {
                        const id = newDivider();
                        onChange([...visibleIds, id, ...hiddenIds], { [id]: true });
                      }}
                    >
                      <span className="grid size-5 shrink-0 place-items-center">
                        <Minus className="size-4" />
                      </span>
                      Divider
                    </DropdownMenuItem>
                  </>
                )}
              </DropdownMenuContent>
            </DropdownMenu>
          </li>
        )}
      </ul>
    </div>
  );
}

/* how many a section lists, quiet on its row until pointed at */
function CountPicker<T extends number>({
  label,
  lead,
  unit,
  value,
  counts,
  onChange,
}: {
  label: string;
  lead: string;
  unit: string;
  value: T;
  counts: readonly T[];
  onChange: (count: T) => void;
}) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <button
            type="button"
            aria-label={`${label}: ${value}`}
            className="flex h-6 shrink-0 items-center gap-0.5 rounded-md px-1.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground aria-expanded:bg-accent aria-expanded:text-foreground"
          />
        }
      >
        {lead} <span className="font-mono tabular-nums">{value}</span>
        <ChevronDown className="size-3" />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-40">
        <DropdownMenuRadioGroup
          value={String(value)}
          onValueChange={(next) => {
            const count = counts.find((c) => String(c) === next);
            if (count !== undefined) onChange(count);
          }}
        >
          {counts.map((count) => (
            <DropdownMenuRadioItem key={count} value={String(count)}>
              {count} {unit}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

export function CustomizeNavDialog() {
  const open = useStore(sheet, (s) => s.open);
  const customizable = useFlag("customize-nav");
  const layout = useNavLayout();
  const groups = {
    items: arrange(useNavCandidates(), layout),
    sections: arrange(useNavSectionCandidates(), layout),
    footer: arrange(useNavFooterCandidates(), layout),
  };
  const status = layout.shown[NAV_STATUS.id] ?? true;
  const ids = (group: keyof typeof groups) => groups[group].map(({ row }) => row.id);
  /* the whole arrangement is saved on any change, so a later default never
     moves an entry the person placed */
  const change =
    (group: keyof typeof groups) =>
    (order: string[], changed: Record<string, boolean>) => {
      const shown: Record<string, boolean> = {
        ...Object.fromEntries(
          [...groups.items, ...groups.sections, ...groups.footer].map(
            ({ row, shown }) => [row.id, shown],
          ),
        ),
        [NAV_STATUS.id]: status,
        ...changed,
      };
      /* a removed divider is gone, not hidden: any number can be added */
      const gone = new Set(
        Object.keys(shown).filter((id) => isDivider({ id }) && shown[id] === false),
      );
      for (const id of gone) delete shown[id];
      preferences.setNavLayout({
        order: (["items", "sections", "footer"] as const)
          .flatMap((g) => (g === group ? order : ids(g)))
          .filter((id) => !gone.has(id)),
        shown,
        ...(layout.recent ? { recent: layout.recent } : {}),
        ...(layout.listed ? { listed: layout.listed } : {}),
      });
    };
  const entries = (
    group: keyof typeof groups,
    own = false,
  ): { entry: Entry; shown: boolean }[] =>
    groups[group].map(({ row, shown }) => ({
      entry: isDivider(row)
        ? {
            id: row.id,
            label: row.label,
            icon: <Minus className="size-4" />,
            own: true,
          }
        : own
          ? { ...row, own: true }
          : row,
      shown,
    }));
  const isDefault =
    layout.order.length === 0 &&
    Object.keys(layout.shown).length === 0 &&
    layout.recent === undefined &&
    layout.listed === undefined;
  const recent = layout.recent ?? DEFAULT_RECENT;

  return (
    <Dialog
      open={open && customizable}
      onOpenChange={(next) => !next && sheet.setState({ open: false })}
    >
      <DialogContent
        /* a long list scrolls between the title and the buttons */
        className="grid-rows-[auto_minmax(0,1fr)_auto] overflow-hidden sm:max-w-md"
        data-testid="customize-nav"
        /* light, unblurred: the nav behind shows each change as it is made */
        overlayProps={{
          className: "bg-black/10 supports-backdrop-filter:backdrop-blur-none",
        }}
      >
        <DialogHeader>
          <DialogTitle>Customize sidebar</DialogTitle>
          <DialogDescription>
            Drag to reorder, remove what you don't use, and add it back from Add. Saved
            on this device.
          </DialogDescription>
        </DialogHeader>
        <div className="-mx-2 grid min-h-0 gap-4 overflow-y-auto overscroll-contain px-2">
          <NavGroup
            title="Items"
            entries={entries("items")}
            onChange={change("items")}
            newDivider={() => newDividerId(layout.order)}
          />
          <NavGroup
            title="Sections · in the expanded sidebar"
            entries={entries("sections", true)}
            onChange={change("sections")}
            newDivider={() => newDividerId(layout.order, "sections")}
            extra={(entry) =>
              entry.id === "recent" ? (
                <CountPicker
                  label="Recent sessions shown"
                  lead="last"
                  unit="sessions"
                  value={recent}
                  counts={RECENT_COUNTS}
                  onChange={(count) =>
                    preferences.setNavLayout({ ...layout, recent: count })
                  }
                />
              ) : (
                mailboxGroupOf(entry.id) && (
                  <CountPicker
                    label={`${entry.label} items shown`}
                    lead="first"
                    unit="items"
                    value={layout.listed?.[entry.id] ?? DEFAULT_LISTED}
                    counts={LISTED_COUNTS}
                    onChange={(count) =>
                      preferences.setNavLayout({
                        ...layout,
                        listed: { ...layout.listed, [entry.id]: count },
                      })
                    }
                  />
                )
              )
            }
          />
          <NavGroup
            title="Bottom"
            lead={[{ entry: { ...NAV_STATUS, own: true }, shown: status }]}
            entries={entries("footer")}
            onChange={change("footer")}
          />
          <p className="m-0 text-xs text-muted-foreground">
            Settings always stays at the foot of the nav.
          </p>
        </div>
        <DialogFooter>
          <Button
            variant="ghost"
            disabled={isDefault}
            onClick={() => preferences.resetNavLayout()}
          >
            Restore defaults
          </Button>
          <Button onClick={() => sheet.setState({ open: false })}>Done</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
