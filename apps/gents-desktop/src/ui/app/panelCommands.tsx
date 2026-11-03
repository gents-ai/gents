import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";
import { Maximize2, PanelRight, Rows3, X } from "lucide-react";
import { Dialog, DialogContent, DialogTitle } from "@gents/ui/components/dialog";
import { Kbd } from "@gents/ui/components/kbd";
import { cn } from "@gents/ui/lib/utils";
import { currentRoute, type Route } from "@/lib/router";
import { SEARCH_FIELD } from "@/lib/textAssist";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";
import { registerCommand, useCommand } from "@/commands/commands";
import { matchScore, queryWords } from "@/lib/search";
import { dockTabId } from "./Dock";
import { NTH_PANEL, NTH_PANEL_TEXT } from "./panelKeys";
import { dockView, inScope } from "./dock-scope";
import { listSurfaces, useSurfaces, type Surface } from "./surfaces";
import type { ShellLayout } from "./useShellLayout";
import { dockScope, useDockFor, workspace } from "./workspace";

const PANEL = '[data-testid="dock"], [data-testid="dock-tabs"]';

/* the panel renders a frame after a key opens it */
function focusWhenShown(find: () => HTMLElement | null, frames = 10) {
  const el = find();
  if (el) el.focus();
  else if (frames > 0) requestAnimationFrame(() => focusWhenShown(find, frames - 1));
}

/** focus a panel's tab, or with none on the page (the phone's sheet, or a
    panel showing only what it can open) its first control */
const focusPanel = (surface: string | null) =>
  focusWhenShown(
    () =>
      (surface ? document.getElementById(dockTabId(surface)) : null) ??
      document.querySelector<HTMLElement>(`[data-testid="dock"] button`),
  );

/** The panels a page can show, numbered as the picker and the number keys count them:
    its open tabs in the tab bar's order, then the ones it could open. */
export function panelsOf(route: Route, surfaces: readonly Surface[] = listSurfaces()) {
  const dock = workspace.dock(dockScope(route));
  const view = dockView({ ...dock, open: true }, route.name, surfaces);
  return [
    ...view.tabs.map((s) => ({ surface: s, open: true })),
    ...view.available.map((s) => ({ surface: s, open: false })),
  ];
}

/** Opens a panel on the current page, or focuses its tab when it's open. */
export function openPanel(surfaceId: string) {
  const route = currentRoute();
  if (!route) return false;
  const surface = listSurfaces("dock").find(
    (s) => s.id === surfaceId && inScope(s, route.name),
  );
  if (!surface) return false;
  workspace.openSurface(dockScope(route), surface.id);
  focusPanel(surface.id);
}

const picker = createStore<{ open: boolean }>(() => ({ open: false }));
export const openPanelPicker = () => picker.setState({ open: true });
const closePanelPicker = () => picker.setState({ open: false });

/** The panel's commands: showing and hiding it, choosing a panel from a
    list, going to one by its number, expanding it, and one "Open …"
    command for every panel registered, a plugin's included. */
export function usePanelCommands(route: Route, layout: ShellLayout) {
  const scope = dockScope(route);
  const { dock } = useDockFor(scope);
  const surfaces = useSurfaces();
  /* where focus was before a key opened the panel, to return to on hiding */
  const returnTo = useRef<Element | null>(null);
  const view = dockView(dock, route.name, surfaces);
  useCommand({
    id: "panel.toggle",
    title: "Show or hide the panel",
    group: "Panel",
    keys: {
      mac: { key: "b", code: "KeyB", mod: true, alt: true },
      other: { key: "b", mod: true, shift: true },
    },
    desktopOnly: true,
    icon: PanelRight,
    when: () =>
      view.shown || dockView({ ...dock, open: true }, route.name, surfaces).shown,
    run: () => {
      if (view.shown) {
        const inside = document.activeElement?.closest(PANEL);
        workspace.closeDock(scope);
        const back = returnTo.current;
        if (inside && back instanceof HTMLElement && back.isConnected) back.focus();
        returnTo.current = null;
        return;
      }
      returnTo.current = document.activeElement;
      workspace.reopenDock(scope);
      focusPanel(
        dockView({ ...dock, open: true }, route.name, surfaces).active?.id ?? null,
      );
    },
  });
  useCommand({
    id: "panel.picker",
    title: "Choose a panel",
    group: "Panel",
    keys: {
      mac: { key: "p", code: "KeyP", mod: true, alt: true },
      other: { key: "p", mod: true, shift: true },
    },
    desktopOnly: true,
    icon: Rows3,
    when: () => panelsOf(route, surfaces).length > 0,
    run: openPanelPicker,
  });
  useCommand({
    id: "panel.nth",
    title: "Go to a panel by its number",
    group: "Panel",
    keys: NTH_PANEL,
    keysText: NTH_PANEL_TEXT,
    desktopOnly: true,
    palette: false,
    run: (index) => {
      const panel = panelsOf(route, surfaces)[index];
      if (!panel) return false;
      if (!view.shown) returnTo.current = document.activeElement;
      openPanel(panel.surface.id);
    },
  });
  /* no key: ⌘W always closes the window, as the menu's Close Window does */
  useCommand({
    id: "panel.close-tab",
    title: "Close panel tab",
    group: "Panel",
    icon: X,
    when: () => view.active !== null,
    run: () => {
      const closing = view.active;
      if (!closing) return false;
      workspace.closeTab(scope, closing.id);
      /* the last tab closed leaves the panel on what it can open, as an
         empty panel shows it, rather than hiding it */
      const after = workspace.dock(scope);
      if (after.tabs.length === 0) workspace.reopenDock(scope);
      focusPanel(after.active);
    },
  });
  useCommand({
    id: "panel.expand",
    title: "Expand the panel to fill the window, or restore it",
    group: "Panel",
    icon: Maximize2,
    when: () => view.shown && layout.docked,
    run: () => {
      if (layout.divider.toEnd > 0.5) layout.divider.showPane();
      else layout.divider.hidePane();
    },
  });
  /* "Open …" for each panel, from the panels themselves */
  useEffect(() => {
    const off = surfaces
      .filter((s) => s.placements.includes("dock"))
      .map((s) =>
        registerCommand({
          id: `panel.open.${s.id}`,
          title: `Open ${s.title} panel`,
          group: "Panel",
          icon: s.icon,
          when: () => {
            const here = currentRoute();
            return here !== null && inScope(s, here.name);
          },
          run: () => openPanel(s.id),
        }),
      );
    return () => off.forEach((f) => f());
  }, [surfaces]);
}

/** A list of every panel the page can show, to open one by name or number. */
export function PanelPicker({ route }: { route: Route }) {
  const wanted = useStore(picker, (s) => s.open);
  const input = useRef<HTMLInputElement | null>(null);
  /* opened from the search palette, it waits for the palette to close:
     the palette hands focus back as it goes, which would take it from the
     picker's field */
  const dialog = useExclusivePopover(closePanelPicker);
  const { open, onOpenChange } = dialog;
  useEffect(() => {
    if (wanted !== open) onOpenChange(wanted);
  }, [wanted, open, onOpenChange]);
  return (
    <Dialog
      open={open}
      onOpenChange={onOpenChange}
      onOpenChangeComplete={dialog.onOpenChangeComplete}
    >
      <DialogContent
        ref={dialog.popupRef}
        initialFocus={input}
        showCloseButton={false}
        aria-modal="true"
        className="flex flex-col gap-0 overflow-hidden p-0 sm:max-w-sm"
        style={{
          top: "calc(var(--viewport-top) + var(--viewport-height) * 0.12)",
          translate: "-50% 0",
        }}
        data-testid="panel-picker"
      >
        <DialogTitle className="sr-only">Open a panel</DialogTitle>
        {open && <PickerBody route={route} input={input} />}
      </DialogContent>
    </Dialog>
  );
}

function PickerBody({
  route,
  input,
}: {
  route: Route;
  input: React.RefObject<HTMLInputElement | null>;
}) {
  const surfaces = useSurfaces();
  const panels = useMemo(() => panelsOf(route, surfaces), [route, surfaces]);
  const [query, setQuery] = useState("");
  const shown = useMemo(() => {
    const words = queryWords(query);
    return panels
      .map((p, i) => ({ ...p, number: i + 1 }))
      .filter(
        (p) => !words.length || matchScore(words, p.surface.title.toLowerCase()) > 0,
      );
  }, [panels, query]);
  const [active, setActive] = useState(0);
  useEffect(() => setActive(0), [query]);
  const listId = useId();
  const choose = (surfaceId: string) => {
    closePanelPicker();
    openPanel(surfaceId);
  };
  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const by = e.key === "ArrowDown" ? 1 : -1;
      setActive((i) => (shown.length ? (i + by + shown.length) % shown.length : 0));
    } else if (e.key === "Enter" && shown[active]) {
      e.preventDefault();
      choose(shown[active].surface.id);
    } else if (!query && /^[1-9]$/.test(e.key) && !e.metaKey && !e.ctrlKey) {
      /* with nothing typed, a digit is a panel's number, not a search */
      const panel = panels[Number(e.key) - 1];
      if (!panel) return;
      e.preventDefault();
      choose(panel.surface.id);
    }
  };
  const current = shown[active];
  return (
    <div className="flex min-h-0 flex-col" onKeyDown={onKeyDown}>
      <input
        ref={input}
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        role="combobox"
        aria-label="Find a panel"
        aria-expanded
        aria-controls={listId}
        aria-activedescendant={current ? `${listId}-${current.surface.id}` : undefined}
        placeholder="Find a panel, or press its number"
        className="h-11 w-full bg-transparent px-4 text-sm outline-none placeholder:text-muted-foreground"
        {...SEARCH_FIELD}
      />
      <div
        id={listId}
        role="listbox"
        aria-label="Panels"
        className="max-h-80 overflow-y-auto border-t border-border px-2 py-2"
      >
        {shown.length === 0 && (
          <p className="px-3 py-6 text-center text-sm text-muted-foreground">
            No panel matches “{query.trim()}”
          </p>
        )}
        {shown.map((p, i) => (
          <div
            key={p.surface.id}
            id={`${listId}-${p.surface.id}`}
            role="option"
            aria-selected={i === active}
            onMouseMove={() => setActive(i)}
            onClick={() => choose(p.surface.id)}
            className={cn(
              "flex h-8 cursor-default items-center gap-3 rounded-lg px-3 text-sm",
              i === active && "bg-accent",
            )}
          >
            <p.surface.icon className="size-4 shrink-0 text-muted-foreground" />
            <span className="min-w-0 flex-1 truncate">{p.surface.title}</span>
            {p.open && (
              <span className="shrink-0 text-xs text-muted-foreground">Open</span>
            )}
            {p.number <= 9 && <Kbd className="shrink-0">{p.number}</Kbd>}
          </div>
        ))}
      </div>
    </div>
  );
}
