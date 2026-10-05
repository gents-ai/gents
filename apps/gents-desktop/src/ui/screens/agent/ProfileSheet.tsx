/* A new model drafted beside the page that needs it: the full profile
   editor, nothing saved until Create, and the caller gets the id. */
import { useState } from "react";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { EditorSheet } from "./EditorSheet";
import { ProfileEditor, newProfileDocument } from "./ProfilesPanel";
import { useAccounts } from "./InferencePanel";

export function ProfileSheet({
  deployment,
  open,
  onClose,
  backendId,
}: {
  deployment: DeploymentView;
  open: boolean;
  /* the new profile's id, or null when discarded */
  onClose: (profileId: string | null) => void;
  /* the backend it is added to (a backend's Add profile row) */
  backendId?: string;
}) {
  const { accounts } = useAccounts(deployment.agentDid);
  const [draft, setDraft] = useState(() =>
    newProfileDocument(deployment, backendId, accounts),
  );
  /* drafted as it opens, from the accounts as they are by then */
  const [wasOpen, setWasOpen] = useState(false);
  if (open !== wasOpen) {
    setWasOpen(open);
    if (open) setDraft(newProfileDocument(deployment, backendId, accounts));
  }
  const close = (id: string | null) => {
    onClose(id);
    setDraft(newProfileDocument(deployment, backendId, accounts));
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
          deployment={deployment}
          profile={draft}
          draft={{ onSaved: (id) => close(id), onCancel: () => close(null) }}
          embedded
        />
      )}
    </EditorSheet>
  );
}
