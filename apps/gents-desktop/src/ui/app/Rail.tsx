import { useContext, type ReactNode } from "react";
import { Waypoints } from "lucide-react";
import { Tooltip, TooltipContent, TooltipTrigger } from "@gents/ui/components/tooltip";
import { cn } from "@gents/ui/lib/utils";
import { useMorph } from "@/lib/morph";
import { href, type Route } from "@/lib/router";
import { NodeAvatar, nodeAvatarOf } from "@/screens/AgentAvatar";
import { NodeHoverCard } from "@/screens/HoverCards";
import { Kbd } from "@gents/ui/components/kbd";
import { TOOLTIP_KBD } from "@/screens/Hint";
import { FlyoutOpenContext } from "./RailFlyout";
import { SyncHealth } from "./SyncHealth";
import { navRowState, type NavContext } from "./navRegistry";
import { drawNavRows, useNavFooter, useNavRows, type NavRow } from "./navLayout";
import { NavMenu } from "./CustomizeNav";
import { useKeysHint } from "@/commands/commands";
import { NavScroll } from "./NavScroll";
import { ContribBoundary } from "@/contrib/react/boundary";
import { ICON_PART } from "./sidebarChange";
import {
  useMailboxCount,
  useOnline,
  useSelectedNode,
  useSyncHealth,
} from "@/hooks/useClient";

function RailItem({
  id,
  label,
  command,
  active,
  count,
  to,
  children,
}: {
  id: string;
  label: string;
  /** the command whose key its tooltip names */
  command?: string;
  active?: boolean;
  count?: number;
  to: Route;
  children: ReactNode;
}) {
  const { part } = useMorph();
  const keys = useKeysHint(command ?? "");
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
        <span ref={part(`icon:${id}`)} className={ICON_PART}>
          {children}
        </span>
        {count ? (
          <span className="absolute -end-1.5 -top-1.5 grid h-4 min-w-4 place-items-center rounded-full bg-brand px-1 font-mono text-[10px] leading-none text-brand-foreground">
            {count}
          </span>
        ) : null}
      </TooltipTrigger>
      <TooltipContent side="right">
        {label}
        {keys && <Kbd className={TOOLTIP_KBD}>{keys}</Kbd>}
      </TooltipContent>
    </Tooltip>
  );
}

function RailNode({ route }: { route: Route }) {
  const node = useSelectedNode();
  const online = useOnline();
  const nodeName = node ? (node.node.displayName ?? node.label) : null;
  const { part } = useMorph();
  if (!node)
    return (
      <a
        href={href({ name: "nodes" })}
        aria-label="No active node"
        title="No active node"
        className="mb-2 grid size-7 place-items-center rounded-full border border-dashed border-border text-muted-foreground hover:text-foreground"
      >
        <Waypoints className="size-3.5" />
      </a>
    );
  return (
    <NodeHoverCard deployment={node}>
      <a
        href={href({ name: "agent", nodeDid: node.nodeDid, section: "agent" })}
        aria-label={`${nodeName ?? "Node"} configuration`}
        aria-current={route.name === "agent" ? "page" : undefined}
        ref={part("icon:node")}
        className={cn(
          "relative mb-2 block size-7 rounded-full [view-transition-class:morph-icon] ring-1 ring-border ring-offset-2 ring-offset-background transition-shadow hover:ring-muted-foreground",
          /* the node's own pages and the list of nodes are both its place */
          (route.name === "agent" || route.name === "nodes") && "ring-2 ring-ink",
        )}
      >
        <NodeAvatar
          name={nodeName ?? "Node"}
          {...nodeAvatarOf(node)}
          className="size-7"
        />
        <span
          className={cn(
            "absolute -end-0.5 -bottom-0.5 size-2.5 rounded-full border-2 border-background",
            online ? "bg-brand" : "bg-border",
          )}
          aria-hidden="true"
        />
      </a>
    </NodeHoverCard>
  );
}

function RailSyncDot() {
  const flyoutOpen = useContext(FlyoutOpenContext);
  const syncHealth = useSyncHealth();
  return (
    <div className="relative h-8 w-7">
      <div
        className={cn(
          "absolute z-40",
          /* the dot keeps its place as it becomes the row, which spans the
             flyout's rows */
          flyoutOpen ? "-start-px -top-0.5 w-[275px]" : "start-0 top-0 w-7",
        )}
      >
        <SyncHealth syncHealth={syncHealth} compact={!flyoutOpen} row={flyoutOpen} />
      </div>
    </div>
  );
}

export function Rail({ route, settings }: { route: Route; settings: ReactNode }) {
  const mailboxCount = useMailboxCount();
  const selected = useSelectedNode();
  const rows = useNavRows();
  const footer = useNavFooter();
  const navContext: NavContext = {
    mailboxCount,
    selectedNodeDid: selected?.nodeDid ?? null,
  };
  /* a row a UX plugin contributed has its predicates evaluated apart and
     its icon drawn behind a boundary, so one bad row costs that row; a row
     with nowhere to go right now is not drawn */
  const drawRow = (item: NavRow) => {
    const { to, active, count } = navRowState(
      item.railActive ? { ...item, active: item.railActive } : item,
      route,
      navContext,
    );
    if (!to) return null;
    const drawn = (
      <RailItem
        key={item.id}
        id={item.id}
        label={item.label}
        command={item.command}
        to={to}
        active={active}
        count={count}
      >
        {item.icon}
      </RailItem>
    );
    if (item.own) return drawn;
    return (
      <ContribBoundary key={item.id} id={item.id} variant="chip">
        {drawn}
      </ContribBoundary>
    );
  };
  return (
    /* the flyout and the pinned panel start their node at this height */
    <NavMenu
      render={<nav className="flex h-full flex-col items-center gap-1 pt-[23px]" />}
    >
      <RailNode route={route} />
      <div className="mb-1 h-px w-5 bg-border" />
      {/* the top items scroll when the window is too short for them, and
          the foot stays; the badges overhang their icons, so the list keeps
          room for them, as the panel's does */}
      <NavScroll>
        <div className="flex flex-col items-center gap-2 py-1.5">
          {drawNavRows(rows, drawRow, (id) => (
            <div
              key={id}
              data-testid="nav-divider"
              className="my-1 h-px w-5 bg-border"
            />
          ))}
        </div>
      </NavScroll>
      <div className="flex shrink-0 flex-col items-center gap-1 pb-[17px]">
        {footer.status && <RailSyncDot />}
        <div className="h-px w-8 bg-border" />
        {footer.rows.map(drawRow)}
        {settings}
      </div>
    </NavMenu>
  );
}
