/* The folders plugins may read or write when an agent or a graph names a
   path (for example "read ~/Documents/x.pdf"). The session's working folder
   is always readable; a path elsewhere is asked about, or refused in a
   graph, unless its folder is listed here. */
import { useCallback, useEffect, useState } from "react";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { toast } from "sonner";
import { canPickDirectory, pickDirectory } from "../../lib/pickDirectory";
import { call, message } from "./bridgeCall";
import { Group, Row } from "./rows";

type Access = "read" | "read_write";

interface Folder {
  path: string;
  access: Access;
}

interface Folders {
  dirs: Folder[];
}

const ACCESS_LABEL: Record<Access, string> = {
  read: "Read only",
  read_write: "Read and write",
};

export function AllowedFoldersPanel() {
  const [folders, setFolders] = useState<Folders | null>(null);
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);

  const run = useCallback(
    async (command: string, args: Record<string, unknown> = {}) => {
      setBusy(true);
      try {
        setFolders(await call<Folders>(command, args));
        return true;
      } catch (error) {
        toast.error(message(error));
        return false;
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  useEffect(() => {
    void run("desktop_allowed_dirs_list");
  }, [run]);

  const add = (path: string, access: Access) =>
    run("desktop_allowed_dirs_add", { path, access });

  async function choose() {
    const picked = await pickDirectory({ title: "Allow a folder" });
    if (picked) await add(picked, "read");
  }

  return (
    <div>
      <Group title="Allowed folders">
        {folders === null && <Row label="Loading" />}
        {folders?.dirs.length === 0 && (
          <Row
            label="None added"
            description="The working folder is always readable. Other paths are asked about."
          />
        )}
        {folders?.dirs.map((folder) => (
          <Row key={folder.path} label={folder.path}>
            <div className="flex gap-2">
              <Button
                size="sm"
                variant="outline"
                disabled={busy}
                onClick={() =>
                  add(folder.path, folder.access === "read" ? "read_write" : "read")
                }
              >
                {ACCESS_LABEL[folder.access]}
              </Button>
              <Button
                size="sm"
                variant="outline"
                disabled={busy}
                onClick={() =>
                  run("desktop_allowed_dirs_remove", { path: folder.path })
                }
              >
                Remove
              </Button>
            </div>
          </Row>
        ))}
        <Row label="Add a folder">
          {canPickDirectory() ? (
            <Button size="sm" disabled={busy} onClick={choose}>
              Choose folder
            </Button>
          ) : (
            <form
              className="flex gap-2"
              onSubmit={async (event) => {
                event.preventDefault();
                if (await add(typed.trim(), "read")) setTyped("");
              }}
            >
              <Input
                value={typed}
                onChange={(event) => setTyped(event.target.value)}
                placeholder="/path/to/folder"
                aria-label="Folder path"
              />
              <Button type="submit" size="sm" disabled={busy || !typed.trim()}>
                Add
              </Button>
            </form>
          )}
        </Row>
      </Group>
    </div>
  );
}
