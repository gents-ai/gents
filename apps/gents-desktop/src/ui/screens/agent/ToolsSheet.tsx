/* A new Tools document drafted beside the page that needs it: the full
   editor, nothing saved until Create, and the caller gets the id. */
import { useState } from "react";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { EditorSheet } from "./EditorSheet";
import { ToolsEditor, newToolsDocument } from "./ToolsPanel";

export function ToolsSheet({
  shell,
  deployment,
  open,
  onClose,
}: {
  shell: Shell;
  deployment: DeploymentView;
  open: boolean;
  /* the new document's id, or null when discarded */
  onClose: (toolsId: string | null) => void;
}) {
  const [draft, setDraft] = useState(() => newToolsDocument(deployment));
  const close = (id: string | null) => {
    onClose(id);
    setDraft(newToolsDocument(deployment));
  };
  return (
    <EditorSheet
      open={open}
      onClose={() => close(null)}
      title="New tools"
      description="What a behavior may touch: files, commands, network, remote tools and more."
    >
      {open && (
        <ToolsEditor
          key={draft.tools_id}
          shell={shell}
          deployment={deployment}
          tools={draft}
          draft={{ onSaved: (id) => close(id), onCancel: () => close(null) }}
          embedded
        />
      )}
    </EditorSheet>
  );
}
