/* Each message uses this computer's picked path as the tools' working folder;
   with none, the agent's own folder applies. */
import { ChevronDown, FolderClosed } from "lucide-react";
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
import { useApp } from "../app/AppContext";
import { Hint } from "./Hint";
import { useChatFolder, useRecentFolders } from "../hooks/useClient";
import { useHomeFolder } from "../hooks/useHomeFolder";
import { toastFailure } from "../lib/failure";
import { distinctNames, pathName, shortPath } from "../lib/paths";
import { canPickDirectory, pickDirectory } from "../../lib/nativeShell";

const NONE = "";

export function SelectedChatFolder() {
  const {
    actions: { setChatFolder },
  } = useApp();
  return (
    <ChatFolderPicker
      folder={useChatFolder()}
      recent={useRecentFolders()}
      home={useHomeFolder()}
      onChange={setChatFolder}
    />
  );
}

export function ChatFolderPicker({
  folder,
  recent = [],
  home = null,
  onChange,
}: {
  folder: string | null;
  recent?: string[];
  /** this computer's home folder; every path here was picked on it */
  home?: string | null;
  onChange: (folder: string | null) => void;
}) {
  if (!canPickDirectory()) return null;
  const choose = async () => {
    const picked = await pickDirectory({
      defaultPath: folder,
      title: "Choose the folder to work in",
    });
    if (picked) onChange(picked);
  };
  const value = folder ?? NONE;
  const pick = (next: unknown) =>
    onChange(typeof next === "string" && next ? next : null);
  const names = distinctNames(recent, home);
  return (
    <DropdownMenu>
      <Hint label="Change the working folder for this session">
        <DropdownMenuTrigger
          render={
            <button
              type="button"
              data-testid="chat-folder"
              className="inline-flex h-7 max-w-64 min-w-0 cursor-pointer items-center gap-1.5 rounded-md px-1.5 text-xs text-muted-foreground transition-colors hover:text-foreground data-[popup-open]:text-foreground"
            />
          }
        >
          <FolderClosed className="size-3.5 shrink-0" />
          <span className="truncate">{folder ? pathName(folder) : "No folder"}</span>
          <ChevronDown className="size-3 shrink-0" />
        </DropdownMenuTrigger>
      </Hint>
      <DropdownMenuContent
        align="start"
        side="top"
        className="w-64 text-xs [&_[role^=menuitem]]:text-xs"
      >
        {folder && (
          <>
            <DropdownMenuGroup>
              <DropdownMenuLabel
                className="truncate font-mono text-muted-foreground"
                title={folder}
              >
                {shortPath(folder, { home, max: 34 })}
              </DropdownMenuLabel>
            </DropdownMenuGroup>
            <DropdownMenuSeparator />
          </>
        )}
        {recent.length > 0 && (
          <>
            <DropdownMenuGroup>
              <DropdownMenuLabel>Recent</DropdownMenuLabel>
              <DropdownMenuRadioGroup value={value} onValueChange={pick}>
                {recent.map((path) => {
                  const { name, where } = names.get(path)!;
                  return (
                    <DropdownMenuRadioItem
                      key={path}
                      value={path}
                      title={path}
                      closeOnClick
                    >
                      <FolderClosed className="size-3.5 shrink-0 text-muted-foreground" />
                      <span className="truncate">{name}</span>
                      {where && (
                        <span className="min-w-0 truncate text-muted-foreground">
                          {where}
                        </span>
                      )}
                    </DropdownMenuRadioItem>
                  );
                })}
              </DropdownMenuRadioGroup>
            </DropdownMenuGroup>
            <DropdownMenuSeparator />
          </>
        )}
        <DropdownMenuGroup>
          <DropdownMenuItem
            onClick={() =>
              choose().catch((e: unknown) => toastFailure("choose a folder", e))
            }
          >
            Choose a folder…
          </DropdownMenuItem>
          <DropdownMenuRadioGroup value={value} onValueChange={pick}>
            <DropdownMenuRadioItem value={NONE} closeOnClick>
              No folder
            </DropdownMenuRadioItem>
          </DropdownMenuRadioGroup>
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
