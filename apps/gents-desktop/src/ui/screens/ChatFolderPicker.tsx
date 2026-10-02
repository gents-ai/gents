/* The folder the user works in for this chat, in the composer's leading
   slot: plugin tools read inside it without a prompt. Shown only where the
   OS folder picker exists. */
import { Folder, X } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { canPickDirectory, pickDirectory } from "../lib/pickDirectory";

export const folderLabel = (path: string) =>
  path.split(/[\\/]/).filter(Boolean).pop() ?? path;

export function ChatFolderPicker({
  folder,
  onChange,
}: {
  folder: string | null;
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
  return (
    <span className="inline-flex items-center" data-testid="chat-folder">
      <Button
        variant="quiet"
        size="sm"
        title={folder ?? "Choose the folder to work in"}
        onClick={() => void choose()}
      >
        <Folder />
        {folder ? folderLabel(folder) : "Folder"}
      </Button>
      {folder && (
        <Button
          variant="quiet"
          size="icon-xs"
          aria-label="Clear the folder"
          onClick={() => onChange(null)}
        >
          <X />
        </Button>
      )}
    </span>
  );
}
