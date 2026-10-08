/* A new behavior drafted beside another page: nothing is saved until Save,
   and the page it came from gets the id. */
import type { NodeView } from "../../../hooks/fleetStore";
import { BehaviorEditor } from "./BehaviorEditor";
import { newBehaviorView } from "./behaviorDraft";
import { EditorSheet } from "./EditorSheet";
import { useState } from "react";

export function BehaviorSheet({
  deployment,
  open,
  onClose,
  enabled = false,
}: {
  deployment: NodeView;
  open: boolean;
  /* the new behavior's id, or null when discarded */
  onClose: (behaviorId: string | null) => void;
  /* from a session's composer: saved enabled */
  enabled?: boolean;
}) {
  /* one draft per opening */
  const [draft, setDraft] = useState(() => newBehaviorView(deployment));
  const close = (id: string | null) => {
    onClose(id);
    setDraft(newBehaviorView(deployment));
  };
  return (
    <EditorSheet
      open={open}
      onClose={() => close(null)}
      title="New behavior"
      description="What it is told, what it may use, and what runs it. Nothing is saved until you save."
    >
      {open && (
        <BehaviorEditor
          key={draft.behaviorId}
          deployment={deployment}
          behavior={draft}
          draft={{ onSaved: (id) => close(id), onCancel: () => close(null), enabled }}
          embedded
        />
      )}
    </EditorSheet>
  );
}
