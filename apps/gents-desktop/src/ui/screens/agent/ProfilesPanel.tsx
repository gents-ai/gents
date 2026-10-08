import type { NodeView } from "../../../hooks/fleetStore";
import { useState } from "react";
import { dependentsWarning } from "./dependents";
import type { InferenceProfile } from "@source-inc/gents-desktop-client";
import { href, navigate } from "@/lib/router";
import { ProfileSheet } from "./ProfileSheet";
import { InferencePanel } from "./InferencePanel";
import { Plus } from "lucide-react";
import type { InferenceBackendView } from "@source-inc/gents-desktop-client";
import type { ListRow } from "./ListDetail";
import { newId } from "./draft";
import { ListDetail } from "./ListDetail";
import { RowMenu } from "./RowMenu";
import { useApp } from "@/app/AppContext";
import { ProfileEditor } from "./ProfileEditor";

/* why a model cannot serve right now */
function modelProblem(deployment: NodeView, p: InferenceProfile): string | null {
  const backend = deployment.inferenceBackends.find(
    (b) => b.backendId === p.backend_id,
  );
  if (!backend) return "Backend is missing";
  if (backend.enabled === false) return "Backend is disabled";
  if (backend.authKind === "api_key" && !backend.apiKeyConfigured)
    return "Backend has no API key";
  return null;
}

export function ProfilesPanel({
  deployment,
  item,
}: {
  deployment: NodeView;
  item?: string;
}) {
  const { changeConfig } = useApp().actions;
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "profiles",
  };
  /* Add profile under a backend: the dialog opens on it */
  const [creating, setCreating] = useState<string | null>(null);
  const profileRow = (p: InferenceProfile): ListRow => {
    const problem = modelProblem(deployment, p);
    return {
      id: p.profile_id,
      href: href({ ...base, item: p.profile_id }),
      title: p.display_name ?? p.profile_id,
      meta: (() => {
        const users = deployment.behaviors.filter(
          (b) => b.inferenceProfileId === p.profile_id,
        ).length;
        return `${p.model_name}${users ? ` · ${users} ${users === 1 ? "behavior" : "behaviors"}` : ""}`;
      })(),
      badge: problem ?? undefined,
      badgeTone: "bad" as const,
      tags: p.tags,
      trailing: (
        <RowMenu
          name={p.display_name ?? p.profile_id}
          base={base}
          id={p.profile_id}
          onDuplicate={async () => {
            const profile_id = newId("profile");
            await changeConfig("saveInferenceProfileConfig", {
              document: {
                ...p,
                profile_id,
                display_name: `${p.display_name ?? p.profile_id} copy`,
              },
            });
            return profile_id;
          }}
          onDelete={() =>
            changeConfig("deleteInferenceProfileConfig", {
              profileId: p.profile_id,
              agentDid: deployment.agentDid,
            })
          }
          warning={dependentsWarning(deployment, "profile", p.profile_id)}
        />
      ),
    };
  };
  const modelRows = (b: InferenceBackendView): ListRow[] => [
    ...deployment.inferenceProfiles
      .filter((p) => p.backend_id === b.backendId)
      .map(profileRow),
    {
      id: `add:${b.backendId}`,
      title: "Add profile",
      meta: b.models.length
        ? `${b.models.length} advertised`
        : "probe the backend for models first",
      icon: <Plus className="size-3.5 text-muted-foreground" />,
      onOpen: () => setCreating(b.backendId),
    },
  ];
  if (item) {
    const profile = deployment.inferenceProfiles.find((p) => p.profile_id === item);
    if (profile)
      return (
        <ListDetail
          base={base}
          item={item}
          rows={[
            {
              id: profile.profile_id,
              title: profile.display_name ?? profile.profile_id,
            },
          ]}
          createLabel=""
          empty=""
          detail={() => (
            <ProfileEditor
              key={profile.profile_id}
              deployment={deployment}
              profile={profile}
            />
          )}
        />
      );
  }
  return (
    <>
      <ProfileSheet
        deployment={deployment}
        open={creating !== null}
        backendId={creating ?? undefined}
        onClose={(profileId) => {
          setCreating(null);
          if (profileId) navigate({ ...base, item: profileId });
        }}
      />
      <InferencePanel
        deployment={deployment}
        under={modelRows}
        /* a profile whose backend is gone has no row to sit under; it stays
           listed, with its problem, so it can be repointed or deleted */
        orphans={deployment.inferenceProfiles
          .filter(
            (p) =>
              !deployment.inferenceBackends.some((b) => b.backendId === p.backend_id),
          )
          .map(profileRow)}
      />
    </>
  );
}
