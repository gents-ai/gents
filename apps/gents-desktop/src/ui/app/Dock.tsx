import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { Columns2, Maximize2, MessageSquare, Minimize2, Plus, X } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { Hint } from "@/screens/Hint";
import { PanelMenu } from "./PanelMenu";
import { cn } from "@gents/ui/lib/utils";
import { dockView } from "./dock-scope";
import { useSurfaces, type Placement, type Surface } from "./surfaces";
import { dockScopeOf, useDockFor } from "./workspace";
import { useFleet } from "../hooks/useFleet";
import { useDragReorder } from "../hooks/useDragReorder";

const DOCK_PANEL_ID = "dock-panel";
const dockTabId = (surface: string) => `dock-tab-${surface}`;

export function Dock({
  sessionId,
  routeName,
  placement = "dock",
}: {
  sessionId: string | null;
  routeName: string;
  placement?: Placement;
}) {
  const { dock, closeDock, openSurface } = useDockFor(
    dockScopeOf(routeName, sessionId),
  );
  const view = dockView(dock, routeName, useSurfaces(), placement);
  const last = useRef(view.active);
  if (view.shown && view.active) last.current = view.active;
  const empty = view.shown && !view.active;
  const active = view.shown ? view.active : placement === "dock" ? last.current : null;
  if (!active && !empty) return null;
  const body = active ? (
    <active.render placement={placement} sessionId={sessionId} />
  ) : (
    <OpenAPanel
      available={view.available}
      onOpen={openSurface}
      titled={placement === "dock"}
    />
  );
  if (placement === "sheet") {
    const Icon = active?.icon;
    const title = active?.title ?? "Panels";
    return (
      <aside
        data-testid="dock"
        aria-label={title}
        className="flex h-full min-h-0 flex-col"
      >
        <div className="flex h-12 shrink-0 items-center gap-2 border-b border-border/60 px-4">
          {Icon && <Icon className="size-4 shrink-0 text-muted-foreground" />}
          <span className="min-w-0 flex-1 truncate text-sm font-medium">{title}</span>
          <PanelMenu routeName={routeName} />
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Close panel"
            onClick={() => closeDock()}
          >
            <X />
          </Button>
        </div>
        <div className="min-h-0 flex-1">{body}</div>
      </aside>
    );
  }
  return (
    <aside
      data-testid="dock"
      id={DOCK_PANEL_ID}
      {...(active
        ? { role: "tabpanel", "aria-labelledby": dockTabId(active.id) }
        : { "aria-label": "Panels" })}
      className="flex h-full min-h-0 flex-col rounded-2xl border border-border/60 bg-background"
    >
      <div className="min-h-0 flex-1">{body}</div>
    </aside>
  );
}

/* an open dock with no tab on this page: the panels it can show; the
   phone sheet's header already names them */
function OpenAPanel({
  available,
  onOpen,
  titled,
}: {
  available: readonly Surface[];
  onOpen: (id: string) => void;
  titled: boolean;
}) {
  return (
    <div className="flex h-full min-h-0 overflow-y-auto p-4">
      <div className="m-auto grid w-full max-w-80 gap-3">
        {titled && <h2 className="px-1 text-xs text-muted-foreground">Panels</h2>}
        <div className="grid gap-1.5">
          {available.map((s) => (
            <button
              key={s.id}
              type="button"
              onClick={() => onOpen(s.id)}
              className="flex h-10 cursor-pointer items-center gap-2.5 rounded-xl border border-border/60 px-3 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:bg-accent focus-visible:text-foreground focus-visible:outline-none"
            >
              <s.icon className="size-4 shrink-0" />
              <span className="truncate">{s.title}</span>
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}

export function DockTabs({
  sessionId,
  routeName,
  paneTab = null,
  expanded = false,
  onExpand,
}: {
  sessionId: string | null;
  routeName: string;
  paneTab?: { onShow: () => void; progress: number } | null;
  /** the panel fills the window, the session behind its tab */
  expanded?: boolean;
  onExpand?: () => void;
}) {
  const { dock, activate, moveTab, closeTab, closeDock, openSurface } = useDockFor(
    dockScopeOf(routeName, sessionId),
  );
  const paneTitle =
    useFleet((s) => (sessionId ? s.bySessionId[sessionId]?.title : null)) ?? "Session";
  const tabEls = useRef(new Map<string, HTMLButtonElement>());
  const view = dockView(dock, routeName, useSurfaces(), "dock");
  const reorder = useDragReorder<HTMLDivElement>({
    axis: "x",
    keys: view.tabs.map((tab) => tab.id),
    canMove: () => true,
    commit: (from, to) => {
      const tab = view.tabs[from];
      if (tab) moveTab(tab.id, to);
    },
  });
  /* tabs past the strip's right edge fade out under the + */
  const [cut, setCut] = useState(false);
  const strip = reorder.container;
  useEffect(() => {
    const el = strip.current;
    if (!el) return;
    const measure = () => setCut(el.scrollLeft + el.clientWidth < el.scrollWidth - 1);
    measure();
    const sizes = new ResizeObserver(measure);
    sizes.observe(el);
    el.addEventListener("scroll", measure, { passive: true });
    return () => {
      sizes.disconnect();
      el.removeEventListener("scroll", measure);
    };
  }, [strip, view.tabs.length]);
  const onPress = (e: React.PointerEvent<HTMLElement>, id: string, index: number) => {
    reorder.handle(index).onPointerDown(e);
    if (e.button !== 0) return;
    tabEls.current.get(id)?.focus({ preventScroll: true });
    activate(id);
  };
  const refocus = useRef(false);
  useLayoutEffect(() => {
    if (!refocus.current) return;
    refocus.current = false;
    if (view.active) tabEls.current.get(view.active.id)?.focus();
  });
  const onTabKey = (e: React.KeyboardEvent<HTMLElement>, index: number) => {
    const n = view.tabs.length;
    if (e.key === "Delete") {
      e.preventDefault();
      refocus.current = true;
      closeTab(view.tabs[index]!.id);
      return;
    }
    const to =
      e.key === "ArrowRight"
        ? (index + 1) % n
        : e.key === "ArrowLeft"
          ? (index - 1 + n) % n
          : e.key === "Home"
            ? 0
            : e.key === "End"
              ? n - 1
              : null;
    if (to === null) return;
    e.preventDefault();
    const next = view.tabs[to]!;
    activate(next.id);
    tabEls.current.get(next.id)?.focus();
  };
  const sessionTab = useRef<HTMLButtonElement>(null);
  const [tabWidth, setTabWidth] = useState(0);
  useLayoutEffect(() => {
    if (sessionTab.current) setTabWidth(sessionTab.current.offsetWidth);
  }, [paneTitle, paneTab !== null]);
  if (!view.shown) return null;
  return (
    <div
      data-testid="dock-tabs"
      className="flex h-full min-w-0 flex-1 items-center px-2"
    >
      {paneTab && (
        <div
          className="flex shrink-0 overflow-hidden"
          style={{
            width: tabWidth ? tabWidth * paneTab.progress : undefined,
            marginRight: 8 * paneTab.progress,
            opacity: paneTab.progress,
          }}
        >
          <button
            ref={sessionTab}
            type="button"
            onClick={paneTab.onShow}
            title="Show the session"
            style={{ pointerEvents: paneTab.progress > 0.5 ? undefined : "none" }}
            className="flex h-8 max-w-48 shrink-0 cursor-pointer items-center gap-1.5 rounded-lg border border-border/60 px-2.5 text-xs text-muted-foreground hover:bg-accent hover:text-foreground"
          >
            <MessageSquare className="size-4 shrink-0" />
            <span className="truncate">{paneTitle}</span>
          </button>
        </div>
      )}
      <div className="flex min-w-0 flex-1 items-center gap-1">
        <div
          role="tablist"
          aria-label="Panels"
          ref={reorder.container}
          onDoubleClick={(e) => {
            if (e.target === e.currentTarget) onExpand?.();
          }}
          data-cut={cut || undefined}
          className={cn(
            "flex min-w-0 flex-1 items-center gap-0.5 overflow-x-auto [scrollbar-width:none] [&::-webkit-scrollbar]:hidden",
            cut &&
              "[mask-image:linear-gradient(to_right,black_calc(100%-1.5rem),transparent)]",
          )}
        >
          {view.tabs.map((s, index) => {
            const active = s.id === view.active?.id;
            return (
              <div
                key={s.id}
                role="presentation"
                data-reorder-key={s.id}
                {...reorder.handle(index)}
                onPointerDown={(e) => onPress(e, s.id, index)}
                style={reorder.style(index)}
                className={cn(
                  "group/tab flex h-8 w-56 min-w-18 shrink items-center rounded-lg pr-1 pl-2.5 text-xs transition-colors select-none",
                  active
                    ? "bg-accent text-foreground"
                    : "text-muted-foreground hover:text-foreground",
                  reorder.moving === index && "relative z-10 bg-accent",
                  reorder.lifted === index && "shadow-md ring-1 ring-border",
                  reorder.active && reorder.moving !== index && "pointer-events-none",
                )}
              >
                <button
                  ref={(el) => {
                    if (el) tabEls.current.set(s.id, el);
                    else tabEls.current.delete(s.id);
                  }}
                  type="button"
                  role="tab"
                  id={dockTabId(s.id)}
                  aria-selected={active}
                  aria-controls={active ? DOCK_PANEL_ID : undefined}
                  aria-keyshortcuts="Delete"
                  tabIndex={active ? 0 : -1}
                  onKeyDown={(e) => onTabKey(e, index)}
                  onClick={() => activate(s.id)}
                  className="flex min-w-0 flex-1 cursor-pointer items-center gap-1.5"
                >
                  <s.icon className="size-4 shrink-0" />
                  <span
                    title={s.tab ? s.title : undefined}
                    className={cn("truncate", active && "font-medium")}
                  >
                    {s.tab ?? s.title}
                  </span>
                  <TabBadge surface={s} sessionId={sessionId} />
                </button>
                <button
                  type="button"
                  aria-hidden
                  tabIndex={-1}
                  title={`Close ${s.title}`}
                  onPointerDown={(e) => e.stopPropagation()}
                  onClick={() => closeTab(s.id)}
                  className={cn(
                    "ml-1 grid size-5 shrink-0 cursor-pointer place-items-center rounded-md hover:bg-background",
                    !active &&
                      "opacity-0 group-focus-within/tab:opacity-100 group-hover/tab:opacity-100",
                  )}
                >
                  <X className="size-3.5" />
                </button>
              </div>
            );
          })}
        </div>
        <AddTab available={view.available} onOpen={openSurface} />
      </div>
      {onExpand && (
        <Hint label={expanded ? "Restore panel" : "Expand panel"}>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={expanded ? "Restore panel" : "Expand panel"}
            aria-pressed={expanded}
            className="ml-1 shrink-0"
            onClick={onExpand}
          >
            {expanded ? <Minimize2 /> : <Maximize2 />}
          </Button>
        </Hint>
      )}
      <Hint label="Hide panel">
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Hide panel"
          className="ml-1 shrink-0"
          onClick={() => closeDock()}
        >
          <Columns2 />
        </Button>
      </Hint>
    </div>
  );
}

/* the panels this page can show that aren't open yet; none, no button */
function AddTab({
  available,
  onOpen,
}: {
  available: readonly Surface[];
  onOpen: (id: string) => void;
}) {
  if (available.length === 0) return null;
  return (
    <DropdownMenu>
      <Hint label="Open a panel">
        <DropdownMenuTrigger
          render={
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="Open a panel"
              className="shrink-0"
            />
          }
        >
          <Plus />
        </DropdownMenuTrigger>
      </Hint>
      <DropdownMenuContent
        align="start"
        className="w-48 text-xs [&_[role=menuitem]]:text-xs"
      >
        {available.map((s) => (
          <DropdownMenuItem key={s.id} onClick={() => onOpen(s.id)}>
            <s.icon className="size-4" />
            <span className="flex-1">{s.title}</span>
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function TabBadge({
  surface,
  sessionId,
}: {
  surface: Surface;
  sessionId: string | null;
}) {
  const badge = useFleet(
    useShallow((fleet) => surface.badge?.(fleet, sessionId) ?? null),
  );
  if (!badge) return null;
  const { count, label, mark } = badge;
  return (
    <span
      title={label}
      className="ml-0.5 inline-flex items-center gap-0.5 font-mono text-[10px] leading-none text-muted-foreground"
    >
      <span aria-hidden={label ? true : undefined}>{count}</span>
      {label && <span className="sr-only">{label}</span>}
      {mark && (
        <span
          aria-hidden="true"
          data-testid="tab-badge-mark"
          className={cn(
            "size-1.5 rounded-full",
            mark === "alert" ? "bg-destructive" : "bg-brand",
          )}
        />
      )}
    </span>
  );
}
