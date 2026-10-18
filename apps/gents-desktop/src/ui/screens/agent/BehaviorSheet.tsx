import type { NodeView } from "../../../hooks/fleetStore";
import { AgentEditor } from "./BehaviorEditor";
import { newAgentView } from "./behaviorDraft";
import { EditorSheet } from "./EditorSheet";
import { useState } from "react";

export function AgentSheet({
  deployment,
  open,
  onClose,
  enabled = false,
}: {
  deployment: NodeView;
  open: boolean;
  onClose: (agentId: string | null) => void;
  enabled?: boolean;
}) {
  const [draft, setDraft] = useState(() => newAgentView(deployment));
  const close = (id: string | null) => {
    onClose(id);
    setDraft(newAgentView(deployment));
  };
  return (
    <EditorSheet
      open={open}
      onClose={() => close(null)}
      title="New agent"
      description="What it is told, what it may use, and what runs it. Nothing is saved until you save."
    >
      {open && (
        <AgentEditor
          key={draft.agentId}
          deployment={deployment}
          agent={draft}
          draft={{ onSaved: (id) => close(id), onCancel: () => close(null), enabled }}
          embedded
        />
      )}
    </EditorSheet>
  );
}
