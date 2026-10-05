/* The dock: the right column, holding the surfaces a person opened as
   tabs, one of them showing; the session's menu adds another. The dock owns the
   card and its header, so a surface draws only its body. It draws
   nothing while closed or while none of its tabs apply on this route. */
import { useLayoutEffect, useRef, useState } from "react";
import { Columns2, MessageSquare, X } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Hint } from "@/screens/Hint";
import { useShellContext } from "./ShellContext";
import { PanelMenu } from "./PanelMenu";
import { cn } from "@gents/ui/lib/utils";
import { dockView } from "./dock-scope";
import type { Placement } from "./surfaces";
import { dockScopeOf, useDockFor } from "./workspace";
import { useFleet } from "../hooks/useFleet";

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
  const view = dockView(dock, routeName, placement);
  /* the card keeps showing its last surface while the divider settles shut
     after the store has closed the dock; the shell unmounts it at 0 */
  const last = useRef(view.active);
  if (view.shown && view.active) last.current = view.active;
  const active = view.shown ? view.active : placement === "dock" ? last.current : null;
  if (!active) return null;
  const Body = active.render;
  /* on a phone the dock is one surface in a bottom sheet: its title and a
     close, nothing to reorder or hide; the session's menu switches surfaces */
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
          {/* the sheet is modal, so the session's menu is out of reach: the same menu here switches surfaces */}
          <PanelMenu routeName={routeName} size="icon-sm" />
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

/* The dock's tab strip, drawn by the window bar over the dock's column: the
   opened surfaces as tabs, a session tab while the pane is hidden, and the
   hide button. It shares the workspace store with the dock below it. */
export function DockTabs({
  sessionId,
  routeName,
  paneTab = null,
}: {
  sessionId: string | null;
  routeName: string;
  /* set while the pane is hidden behind a full dock: a tab with the session's
     title at the strip's start brings it back */
  paneTab?: { onShow: () => void; progress: number } | null;
}) {
  const shell = useShellContext();
  const { dock, activate, moveTab, closeTab, closeDock } = useDockFor(
    dockScopeOf(routeName, sessionId),
  );
  const fleet = useFleet(shell, (s) => s);
  const paneTitle =
    (sessionId ? fleet.bySessionId[sessionId]?.title : null) ?? "Session";
  /* reordering, the way a browser does it: a press shows the tab; moved past
     a few pixels it follows the pointer while the others slide aside, and the
     order is written when it is let go. The strip's tabs share one width, so
     a neighbour steps by that width plus the gap. */
  const strip = useRef<HTMLDivElement>(null);
  const tabEls = useRef(new Map<string, HTMLButtonElement>());
  /* geometry is taken at the press, before any tab has moved */
  const press = useRef<{
    id: string;
    startX: number;
    lefts: number[];
    step: number;
  } | null>(null);
  const [drag, setDrag] = useState<{
    id: string;
    from: number;
    to: number;
    dx: number;
  } | null>(null);
  const onPress = (e: React.PointerEvent<HTMLElement>, id: string) => {
    if (e.button !== 0) return;
    /* no text selection rides along with a tab drag (WebKit); that also
       withholds the press's focus, so the tab takes it here */
    e.preventDefault();
    tabEls.current.get(id)?.focus({ preventScroll: true });
    getSelection()?.removeAllRanges();
    document.documentElement.dataset.dragging = "tab";
    const lefts = [
      ...(strip.current?.querySelectorAll<HTMLElement>("[data-tab]") ?? []),
    ].map((el) => el.getBoundingClientRect().left);
    const step = lefts.length > 1 ? lefts[1]! - lefts[0]! : 0;
    press.current = { id, startX: e.clientX, lefts, step };
    e.currentTarget.setPointerCapture(e.pointerId);
    activate(id);
  };
  const onMove = (e: React.PointerEvent<HTMLElement>) => {
    const p = press.current;
    if (!p || !p.step) return;
    const dx = e.clientX - p.startX;
    if (!drag && Math.abs(dx) < 4) return;
    const from = view.tabs.findIndex((t) => t.id === p.id);
    if (from < 0) return;
    const first = p.lefts[0]!;
    const last = p.lefts[p.lefts.length - 1]!;
    /* the tab stays inside the strip; its slot is where its left edge lands */
    const x = Math.max(first, Math.min(last, p.lefts[from]! + dx));
    const to = Math.round((x - first) / p.step);
    setDrag({ id: p.id, from, to, dx: x - p.lefts[from]! });
  };
  const onRelease = () => {
    delete document.documentElement.dataset.dragging;
    if (drag && drag.to !== drag.from) moveTab(drag.id, drag.to);
    press.current = null;
    setDrag(null);
  };
  const shift = (index: number) => {
    const step = press.current?.step ?? 0;
    if (!drag || index === drag.from) return 0;
    if (drag.from < index && index <= drag.to) return -step;
    if (drag.to <= index && index < drag.from) return step;
    return 0;
  };
  const view = dockView(dock, routeName, "dock");
  /* the tablist's keys (APG tabs, automatic activation): arrows, Home and
     End move between tabs and show the one reached; Delete closes the tab,
     and focus follows to the one showing next */
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
  /* the session tab's natural width, so it can grow into place from nothing
     and the tabs beside it slide over rather than jump */
  const sessionTab = useRef<HTMLButtonElement>(null);
  const [tabWidth, setTabWidth] = useState(0);
  useLayoutEffect(() => {
    if (sessionTab.current) setTabWidth(sessionTab.current.offsetWidth);
  }, [paneTitle, paneTab !== null]);
  if (!view.shown || !view.active) return null;
  return (
    <div
      data-testid="dock-tabs"
      className="flex h-full min-w-0 flex-1 items-center gap-1 px-2"
    >
      {paneTab && (
        /* grows and fades in with the divider's last stretch, so it is in
           place by the time the pane is gone; the width is the tab's own,
           measured, scaled by how far along the stretch the divider is */
        <div
          className="flex shrink-0 overflow-hidden"
          style={{
            width: tabWidth ? Math.round(tabWidth * paneTab.progress) : undefined,
            marginRight: Math.round(4 * paneTab.progress),
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
        /* like a browser's tab strip: tabs share the width, up to a comfortable
             size each, shrink as more open, and scroll once they hit their floor */
        ref={strip}
        className="flex min-w-0 flex-1 items-center gap-0.5 overflow-x-auto [scrollbar-width:none] [&::-webkit-scrollbar]:hidden"
      >
        {view.tabs.map((s, index) => {
          const active = s.id === view.active!.id;
          return (
            <div
              key={s.id}
              role="presentation"
              data-tab={s.id}
              onPointerDown={(e) => onPress(e, s.id)}
              onPointerMove={onMove}
              onPointerUp={onRelease}
              onPointerCancel={onRelease}
              onLostPointerCapture={onRelease}
              style={
                drag
                  ? {
                      transform: `translateX(${drag.id === s.id ? drag.dx : shift(index)}px)`,
                      transition:
                        drag.id === s.id ? "none" : "transform 160ms ease-out",
                    }
                  : { transition: "transform 160ms ease-out" }
              }
              className={cn(
                "group/tab flex h-8 min-w-18 max-w-56 flex-1 basis-0 items-center rounded-lg pr-1 pl-2.5 text-xs transition-colors select-none",
                active
                  ? "bg-accent text-foreground"
                  : "text-muted-foreground hover:text-foreground",
                drag?.id === s.id &&
                  "relative z-10 bg-accent shadow-md ring-1 ring-border",
                drag && drag.id !== s.id && "pointer-events-none",
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
                {(() => {
                  const n = s.badge?.(fleet, sessionId) ?? null;
                  return n === null ? null : (
                    <span className="ml-0.5 font-mono text-[10px] leading-none text-muted-foreground">
                      {n}
                    </span>
                  );
                })()}
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
          className="shrink-0"
          onClick={() => closeDock()}
        >
          <Columns2 />
        </Button>
      </Hint>
    </div>
  );
}
