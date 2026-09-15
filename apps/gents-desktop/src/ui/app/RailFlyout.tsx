/* The rail, expanded: hovering the side nav slides out a raised panel
   over the canvas with the same items in full (agent, new session,
   mailbox, sessions) and a quick list of recent sessions, the way a
   collapsed sidebar opens on hover. It overlays rather than pushes, so
   the screen behind never reflows, and its rows sit on the rail's own
   grid (same top padding, heights and gaps, icons in the same 32px slot
   12px from the edge) so nothing appears to move when it opens. Opens
   after a short rest, closes when the pointer leaves; keyboard focus
   inside keeps it open. */
import { useRef, useState, type ReactNode } from "react";
import { ChevronRight, Inbox, Plus, ScrollText, Users } from "lucide-react";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { cn } from "@gents/ui/lib/utils";
import { href, type Route } from "@/lib/router";
import type { NavMode } from "@/nav";
import { AgentAvatar } from "@/screens/AgentAvatar";
import { SessionStatus } from "@/screens/SessionStatus";

const OPEN_AFTER = 260;
const CLOSE_AFTER = 180;
const RECENT = 8;

function Item({
  to,
  active,
  count,
  icon,
  children,
}: {
  to: string;
  active?: boolean;
  count?: number;
  icon: ReactNode;
  children: ReactNode;
}) {
  return (
    <a
      href={to}
      aria-current={active ? "page" : undefined}
      className={cn(
        "mx-3 flex h-8 items-center gap-2 rounded-lg border border-transparent pr-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
        active && "border-border/60 bg-background text-ink shadow-xs",
      )}
    >
      {/* the rail's 32px square, minus the border already on the row */}
      <span className="grid size-[30px] shrink-0 place-items-center">{icon}</span>
      <span className="min-w-0 flex-1 truncate">{children}</span>
      {count ? (
        <span className="grid h-4 min-w-4 place-items-center rounded-full bg-brand px-1 font-mono text-[10px] leading-none text-brand-foreground">
          {count}
        </span>
      ) : null}
    </a>
  );
}

/* the panel's content, shared by the hover flyout, the expanded column
   and the mobile sheet */
export function NavPanel({
  route,
  agentName,
  agentDid,
  deployment,
  online,
  mailboxCount,
  holds = new Set<string>(),
  settings,
}: {
  route: Route;
  agentName: string | null;
  agentDid: string | null;
  deployment: DeploymentView | null;
  online: boolean;
  mailboxCount: number;
  holds?: Set<string>;
  /** the settings menu, as a row at the foot */
  settings?: ReactNode;
}) {
  const recent = [...(deployment?.sessions ?? [])]
    .sort((a, b) => (b.updatedAt ?? "").localeCompare(a.updatedAt ?? ""))
    .slice(0, RECENT);
  const currentSession = route.name === "session" ? route.sessionId : null;
  return (
    <>
      {/* the agent, in the same slot as its avatar on the rail */}
      <a
        href={
          agentDid
            ? href({ name: "agent", agentDid, section: "agent" })
            : href({ name: "agents" })
        }
        aria-current={route.name === "agent" ? "page" : undefined}
        className={cn(
          "group mr-3 mb-2 ml-[14px] flex h-7 items-center gap-3 rounded-lg text-sm transition-colors",
          route.name === "agent" && "text-ink",
        )}
      >
        <span
          className={cn(
            "relative block size-7 shrink-0 rounded-full ring-1 ring-border ring-offset-2 ring-offset-raised transition-shadow group-hover:ring-muted-foreground",
            route.name === "agent" && "ring-2 ring-ink",
          )}
        >
          <AgentAvatar name={agentName ?? "Agent"} className="size-7" />
          <span
            className={cn(
              "absolute -right-0.5 -bottom-0.5 size-2.5 rounded-full border-2 border-raised",
              online ? "bg-brand" : "bg-border",
            )}
            aria-hidden="true"
          />
        </span>
        <span className="min-w-0 flex-1">
          <span className="block truncate font-heading font-medium text-heading">
            {agentName ?? "…"}
          </span>
          <span className="block text-xs text-muted-foreground">Configure</span>
        </span>
      </a>
      {/* the rail's hairline, in place, then drawn on to the edge */}
      <div className="mb-1 ml-[18px] h-px w-5 bg-border" />
      <Item
        to={href({ name: "session", sessionId: null })}
        active={route.name === "session" && route.sessionId === null}
        icon={<Plus className="size-4" />}
      >
        New session
      </Item>
      <Item
        to={href({ name: "mailbox" })}
        active={route.name === "mailbox"}
        count={mailboxCount}
        icon={<Inbox className="size-4" />}
      >
        Mailbox
      </Item>
      <Item
        to={href({ name: "sessions" })}
        active={route.name === "sessions"}
        icon={<ScrollText className="size-4" />}
      >
        Sessions
      </Item>
      {recent.length > 0 && (
        <>
          <p className="mt-2 mb-0 px-5 font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
            Recent sessions
          </p>
          <ScrollArea className="min-h-0">
            <ul className="px-3">
              {recent.map((c) => (
                <li key={c.sessionId}>
                  <a
                    href={href({ name: "session", sessionId: c.sessionId })}
                    aria-current={currentSession === c.sessionId ? "page" : undefined}
                    className={cn(
                      "flex h-8 items-center gap-2.5 rounded-lg px-2 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground",
                      currentSession === c.sessionId && "text-foreground",
                    )}
                  >
                    <SessionStatus
                      turnState={c.turnState}
                      held={holds.has(c.sessionId)}
                    />
                    <span className="min-w-0 flex-1 truncate">
                      {c.title ?? "Untitled session"}
                    </span>
                  </a>
                </li>
              ))}
            </ul>
          </ScrollArea>
          <a
            href={href({ name: "sessions" })}
            className="flex items-center gap-1 px-5 py-1 text-xs text-muted-foreground hover:text-foreground"
          >
            All sessions <ChevronRight className="size-3" />
          </a>
        </>
      )}
      <div className="mt-auto">
        <div className="mx-3 mb-2 h-px bg-border" />
        <Item
          to={href({ name: "agents" })}
          active={route.name === "agents"}
          icon={<Users className="size-4" />}
        >
          Agents
        </Item>
        {settings}
      </div>
    </>
  );
}

export function RailFlyout({
  route,
  agentName,
  agentDid,
  deployment,
  online,
  mailboxCount,
  holds = new Set<string>(),
  mode = "hover",
  settings,
  children,
}: {
  route: Route;
  agentName: string | null;
  agentDid: string | null;
  deployment: DeploymentView | null;
  online: boolean;
  mailboxCount: number;
  /** sessions with a held tool call waiting on a person */
  holds?: Set<string>;
  /** hover: rail with a flyout; expanded: the panel in the flow; collapsed: rail only */
  mode?: NavMode;
  settings?: ReactNode;
  /** the collapsed rail */
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const timer = useRef<number | null>(null);
  const later = (next: boolean, ms: number) => {
    if (timer.current) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setOpen(next), ms);
  };
  if (mode === "collapsed") return <>{children}</>;
  const shown = mode === "expanded" || open;
  const hover = mode === "hover";
  return (
    <div
      className="relative h-full"
      onPointerEnter={(e) =>
        hover && e.pointerType === "mouse" && later(true, OPEN_AFTER)
      }
      onPointerLeave={() => hover && later(false, CLOSE_AFTER)}
      onFocus={() => hover && later(true, 0)}
      onBlur={(e) => {
        if (hover && !e.currentTarget.contains(e.relatedTarget as Node | null))
          later(false, 0);
      }}
      onKeyDown={(e) => hover && e.key === "Escape" && setOpen(false)}
    >
      {hover && children}
      <div
        aria-hidden={!shown}
        className={cn(
          "flex w-64 flex-col gap-2 overflow-hidden bg-raised pt-4 pb-2",
          hover
            ? "absolute -top-px bottom-2 left-0 z-30 rounded-r-2xl border border-l-0 border-border/60 shadow-lg transition-[opacity,transform] duration-200 ease-out"
            : "mb-2 ml-2 h-[calc(100%-0.5rem)] rounded-2xl border border-border/60 shadow-xs",
          hover &&
            (open
              ? "translate-x-0 opacity-100"
              : "pointer-events-none -translate-x-2 opacity-0"),
        )}
      >
        <NavPanel
          route={route}
          agentName={agentName}
          agentDid={agentDid}
          deployment={deployment}
          online={online}
          mailboxCount={mailboxCount}
          holds={holds}
          settings={settings}
        />
      </div>
    </div>
  );
}
