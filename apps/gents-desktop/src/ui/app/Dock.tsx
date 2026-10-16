import { useLayoutEffect, useRef, useState } from "react";
import { Columns2, MessageSquare, X } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Hint } from "@/screens/Hint";
import { PanelMenu } from "./PanelMenu";
import { cn } from "@gents/ui/lib/utils";
import { dockView } from "./dock-scope";
import { useSurfaces, type Placement, type Surface } from "./surfaces";
import { dockScopeOf, useDockFor } from "./workspace";
import { useFleet } from "../hooks/useFleet";
import { useDragReorder } from "../hooks/useDragReorder";

/* the window bar's tabs and the dock's card are drawn apart, and joined by these */
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
  const { dock, closeDock } = useDockFor(dockScopeOf(routeName, sessionId));
  const view = dockView(dock, routeName, useSurfaces(), placement);
  const last = useRef(view.active);
  if (view.shown && view.active) last.current = view.active;
  const active = view.shown ? view.active : placement === "dock" ? last.current : null;
  if (!active) return null;
  const Body = active.render;
  if (placement === "sheet") {
    const Icon = active.icon;
    return (
      <aside
        data-testid="dock"
        aria-label={active.title}
        className="flex h-full min-h-0 flex-col"
      >
        <div className="flex h-12 shrink-0 items-center gap-2 border-b border-border/60 px-4">
          <Icon className="size-4 shrink-0 text-muted-foreground" />
          <span className="min-w-0 flex-1 truncate text-sm font-medium">
            {active.title}
          </span>
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
        <div className="min-h-0 flex-1">
          <Body placement={placement} sessionId={sessionId} />
        </div>
      </aside>
    );
  }
  return (
    <aside
      data-testid="dock"
      id={DOCK_PANEL_ID}
      role="tabpanel"
      aria-labelledby={dockTabId(active.id)}
      className="flex h-full min-h-0 flex-col rounded-2xl border border-border/60 bg-background"
    >
      <div className="min-h-0 flex-1">
        <Body placement={placement} sessionId={sessionId} />
      </div>
    </aside>
  );
}

export function DockTabs({
  sessionId,
  routeName,
  paneTab = null,
}: {
  sessionId: string | null;
  routeName: string;
  paneTab?: { onShow: () => void; progress: number } | null;
}) {
  const { dock, activate, moveTab, closeTab, closeDock } = useDockFor(
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
  if (!view.shown || !view.active) return null;
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
      <div
        role="tablist"
        aria-label="Panels"
        ref={reorder.container}
        className="flex min-w-0 flex-1 items-center gap-0.5 overflow-x-auto [scrollbar-width:none] [&::-webkit-scrollbar]:hidden"
      >
        {view.tabs.map((s, index) => {
          const active = s.id === view.active!.id;
          return (
            <div
              key={s.id}
              role="presentation"
              data-reorder-key={s.id}
              {...reorder.handle(index)}
              onPointerDown={(e) => onPress(e, s.id, index)}
              style={reorder.style(index)}
              className={cn(
                "group/tab flex h-8 min-w-18 max-w-56 flex-1 basis-0 items-center rounded-lg pr-1 pl-2.5 text-xs transition-colors select-none",
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
                <span className={cn("truncate", active && "font-medium")}>
                  {s.title}
                </span>
                <TabBadge surface={s} sessionId={sessionId} />
              </button>
              {/* the close is always there for the tab showing, on hover for
                  the rest. A tablist holds only tabs, so it is the pointer's
                  way to close; the keyboard's is Delete on the tab. */}
              <button
                type="button"
                aria-hidden
                tabIndex={-1}
                title={`Close ${s.title}`}
                /* the tab captures the pointer for dragging, which would
                     redirect this button's click to the tab */
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
      {/* hides the dock and keeps its tabs; the session menu shows it again */}
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

/* a tab's count from the fleet; a number, so the tab re-renders only when
   it changes */
function TabBadge({
  surface,
  sessionId,
}: {
  surface: Surface;
  sessionId: string | null;
}) {
  const n = useFleet((fleet) => surface.badge?.(fleet, sessionId) ?? null);
  return n === null ? null : (
    <span className="ml-0.5 font-mono text-[10px] leading-none text-muted-foreground">
      {n}
    </span>
  );
}
