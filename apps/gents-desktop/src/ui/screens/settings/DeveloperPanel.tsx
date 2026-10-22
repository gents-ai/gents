import { toast } from "sonner";
import { Button } from "@gents/ui/components/button";
import { useApp } from "@/app/AppContext";
import { inNativeShell } from "../../../lib/nativeShell";
import { isMacTauriShell } from "../../../lib/shellPlatform";
import { Group, Row } from "../agent/rows";

export function DeveloperPanel() {
  const { actions, lifecycle } = useApp();
  const openDbExplorer = () => {
    void actions.openDbExplorer().catch((e: unknown) => {
      toast(`DB explorer failed to open: ${String(e)}`);
    });
  };
  const canReset = lifecycle.home.canReset && (!inNativeShell() || isMacTauriShell());
  return (
    <div>
      {actions.canOpenDbExplorer && (
        <Group title="Tools">
          <Row
            label="DB Explorer"
            description="Query the local node's database in its own window."
          >
            <Button variant="outline" onClick={openDbExplorer}>
              Open
            </Button>
          </Row>
        </Group>
      )}
      {canReset && (
        <Group title="Reset">
          <Row
            label="Reset Gents on this Mac"
            description="Stops the local node and removes its home, after showing what would go."
          >
            <Button
              variant="destructive"
              onClick={() => void lifecycle.home.previewReset()}
            >
              Reset…
            </Button>
          </Row>
        </Group>
      )}
      {!actions.canOpenDbExplorer && !canReset && (
        <p className="text-sm text-muted-foreground">
          Nothing to set up here on this computer.
        </p>
      )}
    </div>
  );
}
