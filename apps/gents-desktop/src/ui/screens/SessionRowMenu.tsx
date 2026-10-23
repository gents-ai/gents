import type { ComponentType, ReactElement, ReactNode } from "react";
import {
  ChevronsDownUp,
  ChevronsUpDown,
  Copy,
  CornerLeftUp,
  EllipsisVertical,
  ExternalLink,
  SquareStack,
} from "lucide-react";
import { Button } from "@gents/ui/components/button";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "@gents/ui/components/context-menu";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { href, navigate } from "@/lib/router";
import type { SessionRow } from "@/lib/session-rows";
import { sessionKeyOf } from "../../hooks/fleetStore";

/* a session revealed by "Go to the session that started it": the level it
   lands on, or the one already shown, scrolls it into view once */
let revealing: string | null = null;
export const REVEAL_EVENT = "gents:sessions-reveal";

export function takeReveal(sessionId: string | undefined): boolean {
  if (!sessionId || revealing !== sessionId) return false;
  revealing = null;
  return true;
}

type MenuProps = {
  row: SessionRow;
  /** the session that started it, listed or not */
  starter: SessionSummary | null;
  /** opens or closes its started sessions here; absent where rows can't open */
  onToggle: (() => void) | null;
};

/** a row's menu, from a right-click, a long press or the menu key */
export function SessionRowMenu({
  trigger,
  children,
  ...props
}: MenuProps & { trigger: ReactElement; children: ReactNode }) {
  return (
    <ContextMenu>
      <ContextMenuTrigger render={trigger}>{children}</ContextMenuTrigger>
      <ContextMenuContent className="min-w-56">
        <Items {...props} Item={ContextMenuItem} Separator={ContextMenuSeparator} />
      </ContextMenuContent>
    </ContextMenu>
  );
}

/** the same menu, from the row's actions button */
export function SessionRowActions({
  className,
  ...props
}: MenuProps & { className?: string }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            variant="quiet"
            size="icon-sm"
            aria-label={`${props.row.session.title ?? "Session"} actions`}
            className={className}
          />
        }
      >
        <EllipsisVertical />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-auto min-w-56">
        <Items {...props} Item={DropdownMenuItem} Separator={DropdownMenuSeparator} />
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

type ItemProps = {
  onClick?: () => void;
  nativeButton?: boolean;
  render?: ReactElement;
  children: ReactNode;
};

function Items({
  row,
  starter,
  onToggle,
  Item,
  Separator,
}: MenuProps & {
  Item: ComponentType<ItemProps>;
  Separator: ComponentType;
}) {
  const { session, started, open } = row;
  return (
    <>
      <Item
        nativeButton={false}
        render={<a href={href({ name: "session", sessionId: session.sessionId })} />}
      >
        <ExternalLink />
        Open
      </Item>
      {started > 0 && (
        <Item
          nativeButton={false}
          render={
            <a href={href({ name: "sessionsUnder", session: sessionKeyOf(session) })} />
          }
        >
          <SquareStack />
          Explore started sessions
        </Item>
      )}
      {started > 0 && onToggle && (
        <Item onClick={onToggle}>
          {open ? <ChevronsDownUp /> : <ChevronsUpDown />}
          {open ? "Collapse started sessions" : `Expand started sessions (${started})`}
        </Item>
      )}
      {starter && (
        <Item
          onClick={() => {
            if (row.starter) {
              revealing = starter.sessionId;
              navigate(
                row.starter.level
                  ? { name: "sessionsUnder", session: row.starter.level }
                  : { name: "sessions" },
              );
              window.dispatchEvent(new Event(REVEAL_EVENT));
            } else {
              navigate({ name: "session", sessionId: starter.sessionId });
            }
          }}
        >
          <CornerLeftUp />
          {row.starter
            ? "Go to the session that started it"
            : "Open the session that started it"}
        </Item>
      )}
      <Separator />
      <Item onClick={() => void navigator.clipboard?.writeText(session.sessionId)}>
        <Copy />
        Copy session ID
      </Item>
    </>
  );
}
