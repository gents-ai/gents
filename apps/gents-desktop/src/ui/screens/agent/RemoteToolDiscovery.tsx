import { useState } from "react";
import type { ToolServiceToolView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { DocumentSelection } from "./DocumentSelection";

export function RemoteToolDiscovery({
  discover,
  selected,
  onChange,
}: {
  discover: () => Promise<ToolServiceToolView[]>;
  selected: string[];
  onChange: (values: string[]) => void;
}) {
  const [tools, setTools] = useState<ToolServiceToolView[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <div className="p-4">
      <Button
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          setError(null);
          try {
            setTools(await discover());
          } catch (error) {
            setError(error instanceof Error ? error.message : String(error));
          } finally {
            setBusy(false);
          }
        }}
      >
        {busy ? "Discovering…" : "Discover remote tools"}
      </Button>
      {error && (
        <p role="alert" className="mt-2 text-sm text-destructive">
          {error}
        </p>
      )}
      {tools && (
        <DocumentSelection
          label="Discovered tools"
          options={tools.map((tool) => ({
            value: tool.name,
            label: tool.name,
            description: tool.description,
          }))}
          selected={selected}
          onChange={onChange}
        />
      )}
    </div>
  );
}
