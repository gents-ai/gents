import { useRef, type ComponentType, type ReactElement, type ReactNode } from "react";
import { toast } from "sonner";
import {
  Archive,
  ArchiveRestore,
  Copy,
  FileText,
  Fingerprint,
  Folder,
  Pencil,
  Pin,
  PinOff,
} from "lucide-react";
import {
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuSub,
  ContextMenuSubContent,
  ContextMenuSubTrigger,
} from "@gents/ui/components/context-menu";
import {
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
} from "@gents/ui/components/dropdown-menu";
import type { SessionSummary } from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { copyLater } from "@/lib/clipboard";
import { toastFailure } from "@/lib/failure";
import { sessionMarkdown } from "@/lib/session-markdown";
import { preferences, usePinnedSessions } from "@/preferences";
import { folderOf } from "../../hooks/chatFolders";
import { sessionKeyOf } from "../../hooks/fleetStore";

type ItemProps = {
  onClick?: () => void;
  nativeButton?: boolean;
  render?: ReactElement;
  children: ReactNode;
};

/** the kit's parts one menu is drawn with, so a context menu and a
    dropdown can list the same items */
export type MenuParts = {
  Item: ComponentType<ItemProps>;
  Separator: ComponentType;
  Sub: ComponentType<{ children: ReactNode }>;
  SubTrigger: ComponentType<{ children: ReactNode }>;
  SubContent: ComponentType<{ children: ReactNode }>;
};

export const CONTEXT_MENU: MenuParts = {
  Item: ContextMenuItem,
  Separator: ContextMenuSeparator,
  Sub: ContextMenuSub,
  SubTrigger: ContextMenuSubTrigger,
  SubContent: ContextMenuSubContent,
};

export const DROPDOWN_MENU: MenuParts = {
  Item: DropdownMenuItem,
  Separator: DropdownMenuSeparator,
  Sub: DropdownMenuSub,
  SubTrigger: DropdownMenuSubTrigger,
  SubContent: DropdownMenuSubContent,
};

/** A rename started from a menu keeps the focus its field takes; the menu
    would otherwise hand focus back to its trigger as it closes, and the
    field would end on that blur. Pass `finalFocus` to the menu's content. */
export function useRenameFromMenu(start: (() => void) | null) {
  const started = useRef(false);
  return {
    onRename:
      start &&
      (() => {
        started.current = true;
        start();
      }),
    finalFocus: () => {
      const back = !started.current;
      started.current = false;
      return back;
    },
  };
}

/** what can be done to a session wherever it shows: its row and its header */
export function SessionItems({
  session,
  onRename,
  Item,
  Separator,
  Sub,
  SubTrigger,
  SubContent,
}: MenuParts & {
  session: SessionSummary;
  /** starts its rename in place */
  onRename: (() => void) | null;
}) {
  const { stores, actions } = useApp();
  const key = sessionKeyOf(session);
  const pinnedKeys = usePinnedSessions();
  const pinned = pinnedKeys.includes(key);
  const folder = folderOf(stores.chat.getState().folders, session.sessionId);
  const title = session.title ?? "Untitled session";

  const copyMarkdown = () => {
    copyLater(
      actions
        .readTranscript(session.nodeDid, session.sessionId)
        .then((rows) => sessionMarkdown(title, rows)),
    ).then(
      () => toast("Copied as Markdown"),
      (e: unknown) => toastFailure("copy the session", e),
    );
  };
  const pin = (next: boolean) => {
    const at = pinnedKeys.indexOf(key);
    preferences.setPinned(key, next);
    toast(next ? "Pinned" : "Unpinned", {
      action: { label: "Undo", onClick: () => preferences.setPinned(key, !next, at) },
    });
  };
  const archive = (target: SessionSummary, archived: boolean) => {
    actions.setSessionArchived(target, archived).then(
      (receipt) => {
        if (!archived || receipt.outcome !== "applied") return;
        toast("Archived", {
          action: {
            label: "Undo",
            onClick: () =>
              archive({ ...target, archiveRevision: receipt.revision }, false),
          },
        });
      },
      (e: unknown) =>
        toastFailure(archived ? "archive the session" : "unarchive the session", e),
    );
  };

  return (
    <>
      {onRename && (
        <Item onClick={onRename}>
          <Pencil />
          Rename
        </Item>
      )}
      <Item onClick={() => pin(!pinned)}>
        {pinned ? <PinOff /> : <Pin />}
        {pinned ? "Unpin" : "Pin"}
      </Item>
      <Sub>
        <SubTrigger>
          <Copy />
          Copy
        </SubTrigger>
        <SubContent>
          <Item onClick={() => void navigator.clipboard?.writeText(session.sessionId)}>
            <Fingerprint />
            Session ID
          </Item>
          {!session.unreadableReason && (
            <Item onClick={copyMarkdown}>
              <FileText />
              As Markdown
            </Item>
          )}
          {folder && (
            <Item onClick={() => void navigator.clipboard?.writeText(folder)}>
              <Folder />
              Working directory
            </Item>
          )}
        </SubContent>
      </Sub>
      <Separator />
      {session.archivedAt ? (
        <Item onClick={() => archive(session, false)}>
          <ArchiveRestore />
          Unarchive
        </Item>
      ) : (
        <Item onClick={() => archive(session, true)}>
          <Archive />
          Archive
        </Item>
      )}
    </>
  );
}
