import {
  Check,
  ChevronDown,
  ChevronsUpDown,
  ChevronUp,
  FolderClosed,
  FolderOpen,
} from "lucide-react";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { cn } from "@gents/ui/lib/utils";
import { shortFolder, type ChangedFile } from "@/lib/turnChanges";
import { toggleFileList, useFileListOpen } from "./changes";
import { Hint } from "./Hint";
import { FileCounts, FileLabel, fileName } from "./TurnChanges";

/** Moves between a turn's files: previous and next, a menu of them all
    behind where the reader is, and, where the panel is wide enough, the
    file list's toggle. Each file's own header names it, so this does not. */
export function ChangesPicker({
  files,
  within,
  index,
  onChoose,
}: {
  files: readonly ChangedFile[];
  within: string;
  index: number;
  onChoose: (path: string) => void;
}) {
  const listOpen = useFileListOpen();
  const current = files[index]!;
  return (
    <div className="flex shrink-0 items-center gap-0.5 border-b border-border px-2 py-1">
      <Button
        variant="ghost"
        size="icon-sm"
        aria-label="Previous file"
        disabled={index === 0}
        onClick={() => onChoose(files[index - 1]!.path)}
      >
        <ChevronUp />
      </Button>
      <Button
        variant="ghost"
        size="icon-sm"
        aria-label="Next file"
        disabled={index === files.length - 1}
        onClick={() => onChoose(files[index + 1]!.path)}
      >
        <ChevronDown />
      </Button>
      <span className="flex-1" />
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button
              variant="ghost"
              size="sm"
              aria-label={`Choose a file, ${fileName(current.path)} of ${files.length}`}
              className="font-normal text-muted-foreground tabular-nums"
            />
          }
        >
          {index + 1} of {files.length}
          <ChevronsUpDown />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-80 max-w-[90vw]">
          <DropdownMenuGroup>
            {within && (
              <DropdownMenuLabel title={within}>
                {shortFolder(within)}
              </DropdownMenuLabel>
            )}
            {files.map((f) => (
              <DropdownMenuItem
                key={f.path}
                onClick={() => onChoose(f.path)}
                className="justify-between gap-3"
              >
                <span className="flex min-w-0 items-center gap-1.5">
                  <Check
                    className={cn("size-3.5 shrink-0", f !== current && "invisible")}
                  />
                  <FileLabel path={f.path} within={within} />
                </span>
                <FileCounts file={f} />
              </DropdownMenuItem>
            ))}
          </DropdownMenuGroup>
        </DropdownMenuContent>
      </DropdownMenu>
      <Hint label={listOpen ? "Hide file list" : "Show file list"}>
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="File list"
          aria-pressed={listOpen}
          className={cn(
            "hidden @lg:inline-flex",
            listOpen && "bg-accent text-foreground",
          )}
          onClick={toggleFileList}
        >
          {listOpen ? <FolderOpen /> : <FolderClosed />}
        </Button>
      </Hint>
    </div>
  );
}
