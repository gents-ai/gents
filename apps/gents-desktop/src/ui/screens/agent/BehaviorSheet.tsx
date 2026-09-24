/* A new behavior drafted beside another page: nothing is saved until Save,
   and the page it came from gets the id. */
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { BehaviorEditor, newBehaviorView } from "./BehaviorsPanel";
import { EditorSheet } from "./EditorSheet";
import { useState } from "react";

export function BehaviorSheet({
  shell,
  deployment,
  open,
  onClose,
  enabled = false,
}: {
  shell: Shell;
  deployment: DeploymentView;
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
          shell={shell}
          deployment={deployment}
          behavior={draft}
          draft={{ onSaved: (id) => close(id), onCancel: () => close(null), enabled }}
          embedded
        />
      )}
    </EditorSheet>
  );
}
