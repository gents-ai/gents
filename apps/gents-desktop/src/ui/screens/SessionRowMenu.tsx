import type { ReactElement, ReactNode } from "react";
import {
  ChevronsDownUp,
  ChevronsUpDown,
  CornerLeftUp,
  EllipsisVertical,
  ExternalLink,
  SquareStack,
} from "lucide-react";
import { Button } from "@gents/ui/components/button";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuTrigger,
} from "@gents/ui/components/context-menu";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { href, navigate } from "@/lib/router";
import type { SessionRow } from "@/lib/session-rows";
import { sessionKeyOf } from "../../hooks/fleetStore";
import {
  CONTEXT_MENU,
  DROPDOWN_MENU,
  SessionItems,
  useRenameFromMenu,
  type MenuParts,
} from "./SessionMenu";

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
  /** starts its rename in the row */
  onRename: (() => void) | null;
};

/** a row's menu, from a right-click, a long press or the menu key */
export function SessionRowMenu({
  trigger,
  children,
  onRename: start,
  ...props
}: MenuProps & { trigger: ReactElement; children: ReactNode }) {
  const { onRename, finalFocus } = useRenameFromMenu(start);
  return (
    <ContextMenu>
      <ContextMenuTrigger render={trigger}>{children}</ContextMenuTrigger>
      <ContextMenuContent className="min-w-56" finalFocus={finalFocus}>
        <Items {...props} onRename={onRename} {...CONTEXT_MENU} />
      </ContextMenuContent>
    </ContextMenu>
  );
}

/** the same menu, from the row's actions button */
export function SessionRowActions({
  className,
  onRename: start,
  ...props
}: MenuProps & { className?: string }) {
  const { onRename, finalFocus } = useRenameFromMenu(start);
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
      <DropdownMenuContent
        align="end"
        className="w-auto min-w-56"
        finalFocus={finalFocus}
      >
        <Items {...props} onRename={onRename} {...DROPDOWN_MENU} />
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function Items({ row, starter, onToggle, onRename, ...parts }: MenuProps & MenuParts) {
  const { Item, Separator } = parts;
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
      <SessionItems session={session} onRename={onRename} {...parts} />
    </>
  );
}
