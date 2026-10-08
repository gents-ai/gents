/* UX plugins for this desktop: what is loaded, from which door, and whether
   it is on. A toggle is live (no reload); Reload re-reads every runtime
   plugin from disk; Reveal opens the file. A row that failed shows why. */
import { useState } from "react";
import { Button } from "@gents/ui/components/button";
import { Switch } from "@gents/ui/components/switch";
import { toast } from "sonner";
import { revealInFolder, revealInFolderLabel } from "../../../lib/shellPlatform";
import { reloadRuntimeUxPlugins } from "@/contrib/runtime-door";
import {
  setUxPluginEnabled,
  useUxDecisions,
  useUxPluginRecords,
  type UxPluginRecord,
} from "@/contrib/plugins-store";
import { Group, Row } from "./rows";

const DOOR_LABEL: Record<UxPluginRecord["door"], string> = {
  bundled: "Built in",
  dev: "Dev folder",
  pack: "Pack",
};

function describe(record: UxPluginRecord): string {
  const parts = [DOOR_LABEL[record.door]];
  if (record.pack) parts.push(record.pack);
  if (record.producer === "afb") parts.push("produced by .afb");
  if (record.description) parts.push(record.description);
  return parts.join(" · ");
}

export function UxPluginsPanel() {
  const records = useUxPluginRecords();
  const decisions = useUxDecisions();
  const [busy, setBusy] = useState(false);
  const rows = Object.values(records).sort((a, b) => a.id.localeCompare(b.id));
  const reveal = revealInFolderLabel();

  async function reload() {
    setBusy(true);
    try {
      await reloadRuntimeUxPlugins();
      toast("UX plugins reloaded");
    } catch (error) {
      toast.error(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div data-testid="ux-plugins-panel">
      <Group
        title="UX plugins"
        action={
          <Button
            size="sm"
            variant="outline"
            disabled={busy}
            onClick={() => void reload()}
          >
            Reload
          </Button>
        }
      >
        {rows.length === 0 && (
          <Row
            label="None loaded"
            description="Drop a plugin.js into <home>/ux-plugins/<id>/ or install a pack that ships one, then Reload."
          />
        )}
        {rows.map((record) => (
          <Row
            key={record.id}
            label={
              <span className="flex items-center gap-2">
                {record.name}
                <span className="font-mono text-[10px] text-muted-foreground">
                  {record.id}
                </span>
              </span>
            }
            description={describe(record)}
            error={record.status === "error" ? record.error : undefined}
          >
            <div
              className="flex items-center gap-2"
              data-testid={`ux-plugin-row-${record.id}`}
            >
              {record.file && reveal && (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => void revealInFolder(record.file!).catch(() => {})}
                >
                  {reveal}
                </Button>
              )}
              <Switch
                aria-label={`Enable ${record.name}`}
                checked={record.status === "loaded"}
                disabled={record.status === "error" && !(record.id in decisions)}
                onCheckedChange={(checked) =>
                  void setUxPluginEnabled(record.id, checked).catch((error: unknown) =>
                    toast.error(error instanceof Error ? error.message : String(error)),
                  )
                }
              />
            </div>
          </Row>
        ))}
      </Group>
    </div>
  );
}
