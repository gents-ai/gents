/* A new model drafted beside the page that needs it: the full profile
   editor, nothing saved until Create, and the caller gets the id. */
import { useState } from "react";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { EditorSheet } from "./EditorSheet";
import { ProfileEditor, newProfileDocument } from "./ProfilesPanel";

export function ProfileSheet({
  shell,
  deployment,
  open,
  onClose,
  backendId,
}: {
  shell: Shell;
  deployment: DeploymentView;
  open: boolean;
  /* the new profile's id, or null when discarded */
  onClose: (profileId: string | null) => void;
  /* the backend it is added to (a backend's Add profile row) */
  backendId?: string;
}) {
  const [draft, setDraft] = useState(() => newProfileDocument(deployment, backendId));
  const [openedFor, setOpenedFor] = useState(backendId);
  if (open && openedFor !== backendId) {
    setOpenedFor(backendId);
    setDraft(newProfileDocument(deployment, backendId));
  }
  const close = (id: string | null) => {
    onClose(id);
    setDraft(newProfileDocument(deployment, backendId));
  };
  return (
    <EditorSheet
      open={open}
      onClose={() => close(null)}
      title="New profile"
      description="A backend, a model and its defaults. Nothing is saved until you create it."
    >
      {open && (
        <ProfileEditor
          key={draft.profile_id}
          shell={shell}
          deployment={deployment}
          profile={draft}
          draft={{ onSaved: (id) => close(id), onCancel: () => close(null) }}
          embedded
        />
      )}
    </EditorSheet>
  );
}
