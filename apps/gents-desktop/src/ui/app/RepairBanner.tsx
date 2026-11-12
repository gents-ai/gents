import { Button } from "@gents/ui/components/button";
import { Notice } from "../components/Notice";
import { useApp } from "./AppContext";

export function RepairBanner() {
  const { stores } = useApp();
  const notes = stores.client.use.repairNotes();
  if (notes.length === 0) return null;
  return (
    <Notice
      className="m-3 mb-0 shrink-0"
      tone="info"
      title="Gents repaired your local node"
      description={notes.join(" ")}
      actions={
        <Button
          variant="ghost"
          aria-label="Dismiss"
          onClick={() => stores.client.setState({ repairNotes: [] })}
        >
          ×
        </Button>
      }
    />
  );
}
