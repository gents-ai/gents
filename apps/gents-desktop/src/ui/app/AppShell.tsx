/* The App Shell from the Branding file: a header with the mark and
   settings; a rail with the agent, new session, mailbox and
   sessions; the content slot on the ground. Chrome and canvas share the
   ground; content that needs a surface brings its own. */
import type { ReactElement, ReactNode } from "react";
import {
  CircleAlert,
  Inbox,
  Menu,
  Plus,
  ScrollText,
  Waypoints,
  SlidersHorizontal,
  X,
  ArrowLeft,
  ArrowRight,
} from "lucide-react";
import { useContext, useEffect, useRef, useState } from "react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { Button } from "@gents/ui/components/button";
import { Tooltip, TooltipContent, TooltipTrigger } from "@gents/ui/components/tooltip";
import { cn } from "@gents/ui/lib/utils";
import { href, type History, type Route } from "@/lib/router";
import { applyTheme, themePreference, type ThemePreference } from "@/theme";
import { navPreference, saveNavPreference, type NavMode } from "@/nav";
/* the surfaces this app registers, wherever the shell is rendered */
import "@/screens/surfaces";
import { Dock, DockTabs } from "./Dock";
import { dockView } from "./dock-scope";
import { PaneBarSlotContext } from "./PaneBar";
import { dockScope, useDockFor } from "./workspace";
import { useDivider } from "@/lib/divider";
import { ROOMY_WINDOW, useMediaQuery } from "@/lib/media";
import {
  headerIsWindowBar,
  isWindowsTauriShell,
  trafficLightsInWebview,
} from "../../lib/shellPlatform";
import { AgentAvatar } from "@/screens/AgentAvatar";
import { AgentHoverCard } from "@/screens/HoverCards";
import type {
  SessionSummary,
  DeploymentView,
  SyncHealthView,
} from "@source-inc/gents-desktop-client";
import { Hint } from "@/screens/Hint";
import { Mark } from "./Mark";
import { SyncHealth } from "./SyncHealth";
import { WindowControls } from "./WindowControls";
import { FlyoutOpenContext, NavPanel, RailFlyout } from "./RailFlyout";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";
import {
  Sheet,
  SheetContent,
  SheetTitle,
  SheetTrigger,
} from "@gents/ui/components/sheet";
import { SwipeHandles } from "./SwipeHandles";

function RailItem({
  label,
  active,
  count,
  to,
  children,
}: {
  label: string;
  active?: boolean;
  count?: number;
  to: Route;
  children: ReactNode;
}) {
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <a
            href={href(to)}
            aria-label={label}
            aria-current={active ? "page" : undefined}
            className={cn(
              "relative grid size-8 place-items-center rounded-lg border border-transparent text-muted-foreground transition-colors hover:text-foreground",
              active && "border-border/60 bg-raised text-ink shadow-xs",
            )}
          />
        }
      >
        {children}
        {count ? (
          <span className="absolute -top-1.5 -right-1.5 grid h-4 min-w-4 place-items-center rounded-full bg-brand px-1 font-mono text-[10px] leading-none text-brand-foreground">
            {count}
          </span>
        ) : null}
      </TooltipTrigger>
      <TooltipContent side="right">{label}</TooltipContent>
    </Tooltip>
  );
}

/* global settings live behind the sliders: for now, the theme */
function SettingsMenu({
  nav,
  onNav,
  showNav,
  onOpenDbExplorer,
  trigger,
  children,
}: {
  nav: NavMode;
  onNav: (mode: NavMode) => void;
  /** the side nav choices only make sense where there is a side nav */
  showNav: boolean;
  /** opens the runtime's DB explorer window; absent where the bridge lacks it */
  onOpenDbExplorer?: (() => void) | null;
  trigger: ReactElement;
  children: ReactNode;
}) {
  const [theme, setTheme] = useState<ThemePreference>(themePreference);
  const choose = (next: ThemePreference) => {
    applyTheme(next);
    setTheme(next);
  };
  return (
    <DropdownMenu>
      <DropdownMenuTrigger render={trigger}>{children}</DropdownMenuTrigger>
      <DropdownMenuContent align="start" side="top">
        <DropdownMenuGroup>
          <DropdownMenuLabel>Theme</DropdownMenuLabel>
          <DropdownMenuRadioGroup
            value={theme}
            onValueChange={(v) => choose(v as ThemePreference)}
          >
            <DropdownMenuRadioItem value="light">Light</DropdownMenuRadioItem>
            <DropdownMenuRadioItem value="dark">Dark</DropdownMenuRadioItem>
          </DropdownMenuRadioGroup>
        </DropdownMenuGroup>
        {showNav && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuGroup>
              <DropdownMenuLabel>Side nav</DropdownMenuLabel>
              <DropdownMenuRadioGroup
                value={nav}
                onValueChange={(v) => onNav(v as NavMode)}
              >
                <DropdownMenuRadioItem value="hover">
                  Show on hover
                </DropdownMenuRadioItem>
                <DropdownMenuRadioItem value="expanded">
                  Always expanded
                </DropdownMenuRadioItem>
                <DropdownMenuRadioItem value="collapsed">
                  Collapsed
                </DropdownMenuRadioItem>
              </DropdownMenuRadioGroup>
            </DropdownMenuGroup>
          </>
        )}
        {onOpenDbExplorer && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuGroup>
              <DropdownMenuLabel>Developer</DropdownMenuLabel>
              <DropdownMenuItem onClick={onOpenDbExplorer}>
                DB Explorer
              </DropdownMenuItem>
            </DropdownMenuGroup>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/* The rail's sync chip is one control in both of the rail's states: the
   dot while the rail stands alone, and the same button widened into the
   panel's row form, drawn over the hover flyout's foot, while the flyout
   is open. One element, so nothing swaps under a pointer that opened the
   flyout on its way to it. The standing panel and the phone sheet draw
   their own row; no rail dot exists beside those. */
function RailSyncDot({ syncHealth }: { syncHealth?: SyncHealthView | null }) {
  const flyoutOpen = useContext(FlyoutOpenContext);
  return (
    <div className="relative h-8 w-7">
      <div
        className={cn(
          "absolute top-0 left-0 z-40",
          /* the flyout sits 8px in with 12px row margins: 20px from the
             rail's edge, 6px right of the dot's own slot */
          flyoutOpen ? "left-[6px] w-[16.5rem]" : "w-7",
        )}
      >
        <SyncHealth syncHealth={syncHealth} compact={!flyoutOpen} row={flyoutOpen} />
      </div>
    </div>
  );
}

export function AppShell({
  route,
  history,
  agentName,
  agentDid,
  deployment,
  root,
  ceiling,
  online,
  mailboxCount,
  holds,
  recent,
  working,
  nodeCount,
  syncHealth,
  error,
  onDismissError,
  onReconnect,
  onOpenDbExplorer,
  children,
}: {
  route: Route;
  /** where the person came from and went back from: the bar's own back and forward */
  history?: History;
  agentName: string | null;
  agentDid: string | null;
  deployment: DeploymentView | null;
  root?: string | null;
  ceiling?: string | null;
  online: boolean;
  mailboxCount: number;
  recent?: SessionSummary[];
  working?: DeploymentView | null;
  nodeCount?: number;
  holds?: Set<string>;
  syncHealth?: SyncHealthView | null;
  /** a shell error, shown as a banner over the canvas until dismissed */
  error?: string | null;
  onDismissError?: () => void;
  onReconnect?: () => Promise<void>;
  /** developer option: opens the runtime's DB explorer window */
  onOpenDbExplorer?: (() => void) | null;
  children: ReactNode;
}) {
  const [nav, setNav] = useState<NavMode>(navPreference);
  const { dock, scope: dockOwner, closeDock } = useDockFor(dockScope(route));
  const dockOpen = dockView(dock, route.name).shown;
  /* the dock is the pane's: leaving for a route where none of its tabs
     apply closes it, and it opens again only when asked */
  /* the pane bar's slot: screens fill it through a portal */
  const [paneBar, setPaneBar] = useState<HTMLElement | null>(null);
  const shellEl = useRef<HTMLDivElement>(null);
  /* the mark takes the bar's corner unless the lights do; followed when
     fullscreen toggles, through the root's attributes */
  const [lights, setLights] = useState(() => trafficLightsInWebview());
  useEffect(() => {
    const mo = new MutationObserver(() => setLights(trafficLightsInWebview()));
    mo.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-shell", "data-window-fullscreen"],
    });
    return () => mo.disconnect();
  }, []);
  /* the shell's width, so every column is a length the divider can drive.
     Observed on the shell itself: the window's resize event fires before the
     frame's width variable has caught up, so it would measure the old width */
  const [shellWidth, setShellWidth] = useState(0);
  useEffect(() => {
    const el = shellEl.current;
    if (!el) return;
    const ro = new ResizeObserver(() =>
      setShellWidth(el.getBoundingClientRect().width),
    );
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  const [menuOpen, setMenuOpen] = useState(false);
  const wide = useMediaQuery("(min-width: 768px)");
  /* below md the dock is a sheet, and it takes its turn with the shell's
     popovers like any other dialog (#1778): the store says it is wanted,
     the owner says when it is on screen */
  const dockSheet = useExclusivePopover(closeDock);
  const roomy = useMediaQuery(ROOMY_WINDOW);
  /* the preference is kept; a narrow window shows the rail in its place */
  const shownNav: NavMode = nav === "expanded" && !roomy ? "hover" : nav;
  const rail = wide ? (shownNav === "expanded" ? 304 : 56) : 0;
  /* the dock is a column only where the window has the room; a half-screen
     window shows it as a sheet over the pane, as the side panel was */
  const docked = wide && roomy;
  /* the divider: the dock's width as one continuous number, settling to its
     rest points through a spring; the store says whether the dock is open */
  const divider = useDivider({
    key: "gents-prototype-trace-width",
    initial: 520,
    min: 320,
    paneMin: 360,
    gap: 8,
    container: shellWidth,
    rail,
    open: docked && dockOpen,
    scope: dockOwner,
    onClosed: closeDock,
  });
  const paneHidden = divider.paneHidden;
  /* the bar cross-fades as the dock takes the pane's last stretch: the
     pane's controls go, the session tab comes */
  const toEnd = divider.toEnd;
  /* the dock stays in the grid while it settles shut */
  const dockVisible = docked && (dockOpen || divider.pos > 0);
  const dockCol = dockVisible ? divider.pos + 8 : 0;
  const paneCol = Math.max(0, shellWidth - rail - dockCol);
  const { jumpClosed } = divider;
  const wantSheet = !docked && dockOpen;
  const { onOpenChange: setSheetOpen, open: sheetOpen } = dockSheet;
  useEffect(() => {
    if (wantSheet && !sheetOpen) setSheetOpen(true);
    else if (!wantSheet && sheetOpen) setSheetOpen(false);
  }, [wantSheet, sheetOpen, setSheetOpen]);
  useEffect(() => {
    if (dock.open && !dockView(dock, route.name).shown) {
      closeDock();
      jumpClosed();
    }
  }, [route.name, dock, jumpClosed, closeDock]);
  const markInBar = wide && !lights;
  useEffect(() => {
    document.documentElement.dataset.windowLights = lights ? "inline" : "native";
  }, [lights]);
  const onNav = (mode: NavMode) => {
    saveNavPreference(mode);
    setNav(mode);
  };
  /* settings live at the foot of the nav: an icon on the rail, a row in the panel */
  const railSettings = (
    <SettingsMenu
      nav={nav}
      onNav={onNav}
      showNav={wide}
      onOpenDbExplorer={onOpenDbExplorer}
      trigger={
        <button
          type="button"
          aria-label="Settings"
          className="grid size-8 cursor-pointer place-items-center rounded-lg border border-transparent text-muted-foreground transition-colors hover:text-foreground"
        />
      }
    >
      <SlidersHorizontal className="size-4" />
    </SettingsMenu>
  );
  /* the mark at the rail's top, where the expanded panel and the flyout leave
     room for it, so the nav's rows sit in one place in every mode */
  const mark = (
    <a
      href={href({ name: "agents" })}
      aria-label="Agents"
      className="grid size-7 shrink-0 place-items-center rounded-md bg-ink text-background"
    >
      <Mark className="h-3" />
    </a>
  );
  /* the sync chip at the nav's foot: a row in the panel, the dot on the rail */
  const panelFoot = (
    <div className="mx-3">
      <SyncHealth syncHealth={syncHealth} row />
    </div>
  );
  const panelSettings = (
    <>
      <SettingsMenu
        nav={nav}
        onNav={onNav}
        showNav={wide}
        onOpenDbExplorer={onOpenDbExplorer}
        trigger={
          <button
            type="button"
            aria-label="Settings"
            className="mx-3 flex h-8 w-[calc(100%-1.5rem)] cursor-pointer items-center gap-2 rounded-lg border border-transparent pr-2 text-left text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          />
        }
      >
        <span className="grid size-[30px] shrink-0 place-items-center">
          <SlidersHorizontal className="size-4" />
        </span>
        <span className="min-w-0 flex-1 truncate">Settings</span>
      </SettingsMenu>
    </>
  );
  const windowBar = headerIsWindowBar();
  /* every column is a length so the bar above and the row below share one
     set of tracks, and the divider's motion is the spring's, frame by frame */
  const columns = !wide
    ? "minmax(0,1fr)"
    : shellWidth
      ? `${rail}px ${paneCol}px ${dockCol}px`
      : `${rail}px minmax(0,1fr) ${dockCol}px`;
  return (
    <PaneBarSlotContext.Provider value={paneBar}>
      <div
        className="viewport-frame grid min-h-0 overflow-hidden bg-background text-foreground"
        ref={shellEl}
        data-testid="app-shell"
        data-resize-container=""
        style={{
          gridTemplateColumns: columns,
          gridTemplateRows: "auto minmax(0,1fr)",
        }}
      >
        {/* The window bar: the shell's own strip across the top, on the same
            tracks as the row below. On macOS the traffic lights sit in its
            rail region and on Windows the caption buttons end it; a press on
            it drags the window. The pane region is the slot a screen fills
            (the session's back arrow, title, marks, meter and menu), the dock
            region carries the dock's tabs. Below md it is the app's header:
            the menu and the mark, then the slot. */}
        <header
          className="app-titlebar relative col-span-full row-start-1 grid h-12 min-w-0"
          style={{ gridTemplateColumns: columns }}
          data-tauri-drag-region={windowBar ? "" : undefined}
          data-testid="window-bar"
        >
          {wide && (
            <div
              className="flex items-center justify-center"
              data-tauri-drag-region={windowBar ? "" : undefined}
            >
              {markInBar && mark}
            </div>
          )}
          <div
            className={cn(
              /* clipped: while the pane is hidden its column is 0 and the
                 bar's contents must not spill over the dock's tabs */
              "app-bar-pane flex h-full min-w-0 items-center gap-3 overflow-hidden pr-3",
              /* hidden: no padding either, or a sliver of it would show the arrow's edge */
              paneHidden ? "pointer-events-none px-0" : "app-bar-first",
              /* Windows: whichever region ends the bar leaves room for the caption buttons */
              isWindowsTauriShell() && !(docked && dockOpen) && "pr-[138px]",
            )}
            style={toEnd > 0 && !paneHidden ? { opacity: 1 - toEnd } : undefined}
            aria-hidden={paneHidden || undefined}
            data-tauri-drag-region={windowBar ? "" : undefined}
          >
            {/* below md the rail is gone; the menu opens the same panel as a sheet */}
            <Sheet open={menuOpen} onOpenChange={setMenuOpen}>
              <SheetTrigger
                render={
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    className="shrink-0 md:hidden"
                    aria-label="Menu"
                  />
                }
              >
                <Menu />
              </SheetTrigger>
              <SheetContent
                side="left"
                className="flex w-72 flex-col gap-2 border-border/60 bg-raised pt-4 pb-2"
                onClickCapture={(e) => {
                  /* a link in the panel navigates; the sheet goes with it */
                  if ((e.target as HTMLElement).closest("a[href]")) setMenuOpen(false);
                }}
              >
                <SheetTitle className="sr-only">Navigation</SheetTitle>
                <NavPanel
                  route={route}
                  agentName={agentName}
                  agentDid={agentDid}
                  deployment={deployment}
                  online={online}
                  mailboxCount={mailboxCount}
                  holds={holds}
                  settings={panelSettings}
                  foot={panelFoot}
                  recent={recent}
                  working={working}
                  nodeCount={nodeCount}
                />
              </SheetContent>
            </Sheet>
            <a
              href={href({ name: "agents" })}
              aria-label="Agents"
              className="grid size-7 shrink-0 place-items-center rounded-md bg-ink text-background md:hidden"
            >
              <Mark className="h-3" />
            </a>
            {/* back and forward, the shell's: the way to where the person came
                from, so no screen draws its own back arrow */}
            {history && (
              <div
                className="flex shrink-0 items-center gap-0.5"
                data-testid="history-nav"
              >
                <Hint label="Back">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label="Back"
                    disabled={!history.canBack}
                    onClick={history.back}
                  >
                    <ArrowLeft />
                  </Button>
                </Hint>
                <Hint label="Forward">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label="Forward"
                    disabled={!history.canForward}
                    onClick={history.forward}
                  >
                    <ArrowRight />
                  </Button>
                </Hint>
              </div>
            )}
            {/* what the screen puts here: its title, marks and actions */}
            <div
              ref={setPaneBar}
              className="flex h-full min-w-0 flex-1 items-center gap-2"
            />
            {/* below md the rail is gone: the dot keeps the status in reach */}
            {!wide && <SyncHealth syncHealth={syncHealth} compact />}
          </div>
          {docked && (
            <div
              className={cn(
                "app-bar-dock flex h-full min-w-0 items-center overflow-hidden",
                /* first in the bar while the pane is hidden: it takes the pane's inset */
                paneHidden && "app-bar-first",
                isWindowsTauriShell() && dockOpen && "pr-[138px]",
              )}
              data-tauri-drag-region={windowBar ? "" : undefined}
            >
              {dockOpen && (
                <DockTabs
                  sessionId={route.name === "session" ? route.sessionId : null}
                  routeName={route.name}
                  paneTab={
                    toEnd > 0 ? { onShow: divider.showPane, progress: toEnd } : null
                  }
                />
              )}
            </div>
          )}
          {isWindowsTauriShell() && (
            <div className="absolute inset-y-0 right-0 flex">
              <WindowControls />
            </div>
          )}
        </header>
        {/* the gents chrome: the rail runs the window's full height, the mark
          at its top, so the pane beside it owns its own bar */}
        <div
          className={cn("app-rail col-start-1 row-start-2 min-h-0", !wide && "hidden")}
        >
          <RailFlyout
            mark={markInBar ? null : mark}
            route={route}
            agentName={agentName}
            agentDid={agentDid}
            deployment={deployment}
            online={online}
            mailboxCount={mailboxCount}
            holds={holds}
            recent={recent}
            working={working}
            nodeCount={nodeCount}
            mode={shownNav}
            settings={panelSettings}
            /* the hover flyout's foot is drawn by the rail's own chip */
            foot={shownNav === "expanded" ? panelFoot : undefined}
          >
            <nav className="flex h-full flex-col items-center gap-2 pt-3">
              {/* the rail's mark row keeps its 8px below, which the panel's offset counts */}
              {!markInBar && <div className="mb-2">{mark}</div>}

              {/* the working node: its avatar opens its configuration; a card on
                  hover. With no node on this machine the slot says so and goes
                  to the Nodes list, as the panel's does */}
              {(() => {
                if (!deployment)
                  return (
                    <a
                      href={href({ name: "nodes" })}
                      aria-label="No local node"
                      title="No local node"
                      className="mb-2 grid size-7 place-items-center rounded-full border border-dashed border-border text-muted-foreground hover:text-foreground"
                    >
                      <Waypoints className="size-3.5" />
                    </a>
                  );
                const link = (
                  <a
                    href={
                      agentDid
                        ? href({ name: "agent", agentDid, section: "agent" })
                        : href({ name: "agents" })
                    }
                    aria-label={`${agentName ?? "Agent"} configuration`}
                    aria-current={route.name === "agent" ? "page" : undefined}
                    className={cn(
                      "relative mb-2 block size-7 rounded-full ring-1 ring-border ring-offset-2 ring-offset-background transition-shadow hover:ring-muted-foreground",
                      route.name === "agent" && "ring-2 ring-ink",
                    )}
                  >
                    <AgentAvatar name={agentName ?? "Agent"} className="size-7" />
                    <span
                      className={cn(
                        "absolute -right-0.5 -bottom-0.5 size-2.5 rounded-full border-2 border-background",
                        online ? "bg-brand" : "bg-border",
                      )}
                      aria-hidden="true"
                    />
                  </a>
                );
                return deployment ? (
                  <AgentHoverCard deployment={deployment} root={root} ceiling={ceiling}>
                    {link}
                  </AgentHoverCard>
                ) : (
                  link
                );
              })()}
              <div className="mb-1 h-px w-5 bg-border" />
              <RailItem
                label="New session"
                to={{ name: "session", sessionId: null }}
                active={route.name === "session" && route.sessionId === null}
              >
                <Plus className="size-4" />
              </RailItem>
              <RailItem
                label="Mailbox"
                to={{ name: "mailbox" }}
                active={route.name === "mailbox"}
                count={mailboxCount}
              >
                <Inbox className="size-4" />
              </RailItem>
              <RailItem
                label="Sessions"
                to={{ name: "sessions" }}
                active={
                  route.name === "sessions" ||
                  (route.name === "session" && route.sessionId !== null)
                }
              >
                <ScrollText className="size-4" />
              </RailItem>
              {/* at the foot with settings, where the panel keeps it */}
              {/* 17px: the flyout's 8px inset, its 8px padding and its 1px border, so
                    the foot's items sit exactly where the panel draws them */}
              <div className="mt-auto flex flex-col items-center gap-1 pb-[17px]">
                {/* the rail's dot stays above the flyout, in the row's own
                    place, so a pointer that opened the flyout on its way
                    still lands on a control; while the flyout shows its row
                    the dot fades so there is one. Below md the bar carries it. */}
                {wide && <RailSyncDot syncHealth={syncHealth} />}
                <div className="h-px w-8 bg-border" />
                <RailItem
                  label="Agents"
                  to={{ name: "nodes" }}
                  active={route.name === "nodes" || route.name === "agents"}
                >
                  <Waypoints className="size-4" />
                </RailItem>
                {railSettings}
              </div>
            </nav>
          </RailFlyout>
        </div>
        {/* the pane: a card with no header of its own; its title, marks and
            actions live in the window bar's pane region */}
        <section
          className="flex min-h-0 min-w-0 flex-col md:col-start-2 md:row-start-2 md:mt-2 md:mr-2 md:mb-2 md:overflow-hidden md:rounded-2xl md:border md:border-border/60"
          data-testid="pane"
        >
          <main
            className="@container relative min-h-0 min-w-0 flex-1 overflow-hidden"
            /* past the divider's range the pane keeps its minimum width and
               slides under the dock rather than reflowing narrower */
            style={
              docked && dockVisible && paneCol - 8 < 360 ? { width: 360 } : undefined
            }
          >
            <SwipeHandles />
            {error && (
              <div
                role="alert"
                data-testid="error-banner"
                className="absolute inset-x-6 top-3 z-30 flex items-center gap-3 rounded-2xl border border-destructive/30 bg-raised px-4 py-2.5 shadow-md"
              >
                <CircleAlert className="size-4 shrink-0 text-destructive" />
                <p className="min-w-0 flex-1 truncate text-sm">{error}</p>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => void onReconnect?.()}
                >
                  Reconnect
                </Button>
                <Button
                  size="icon-xs"
                  variant="quiet"
                  aria-label="Dismiss"
                  data-testid="error-banner-dismiss"
                  onClick={onDismissError}
                >
                  <X />
                </Button>
              </div>
            )}
            {children}
          </main>
        </section>
        {docked && (
          <>
            {/* the surface keeps its width inside a clipping cell, so it slides rather than squashes */}
            <div
              className="relative col-start-3 row-start-2 min-h-0 min-w-0 pt-2 pr-2 pb-2"
              data-testid="dock-cell"
              aria-hidden={!dockVisible}
            >
              <div className="h-full overflow-hidden">
                {/* below its minimum the card keeps that width and slides under the edge */}
                <div
                  className="h-full"
                  style={{ width: Math.max(divider.min, divider.pos) }}
                >
                  {dockVisible && (
                    <Dock
                      sessionId={route.name === "session" ? route.sessionId : null}
                      routeName={route.name}
                    />
                  )}
                </div>
              </div>
              {/* the drag handle lives in the gap between the cards: a hairline
                  that darkens on hover; arrow keys resize too */}
              {dockVisible && (
                <div
                  {...divider.handleProps}
                  aria-label="Resize panel"
                  className="group absolute top-0 -left-2 flex h-full w-2 cursor-col-resize items-center justify-center outline-none focus-visible:bg-accent"
                >
                  <div className="h-10 w-0.5 rounded-full bg-border transition-colors group-hover:bg-muted-foreground group-focus-visible:bg-ring" />
                </div>
              )}
            </div>
          </>
        )}
        {/* below md the dock is a bottom sheet showing one surface at a time */}
        {!docked && (
          <Sheet
            open={dockSheet.open}
            onOpenChange={dockSheet.onOpenChange}
            onOpenChangeComplete={dockSheet.onOpenChangeComplete}
          >
            <SheetContent
              ref={dockSheet.popupRef}
              side="bottom"
              showCloseButton={false}
              className="rounded-t-2xl border-t border-border/60 bg-background p-0 data-[side=bottom]:h-[85dvh]"
            >
              <SheetTitle className="sr-only">Side panel</SheetTitle>
              <Dock
                sessionId={route.name === "session" ? route.sessionId : null}
                routeName={route.name}
                placement="sheet"
              />
            </SheetContent>
          </Sheet>
        )}
      </div>
    </PaneBarSlotContext.Provider>
  );
}
