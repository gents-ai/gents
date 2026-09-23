/* A backend's full editor beside the page that references it. */
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { EditorSheet } from "./EditorSheet";
import { BackendEditor, useAccounts } from "./InferencePanel";

export function BackendSheet({
  shell,
  deployment,
  backendId,
  onClose,
}: {
  shell: Shell;
  deployment: DeploymentView;
  backendId: string | null;
  onClose: () => void;
}) {
  const { accounts, reload } = useAccounts(shell, deployment.agentDid);
  const backend =
    deployment.inferenceBackends.find((b) => b.backendId === backendId) ?? null;
  return (
    <EditorSheet
      open={backend !== null}
      onClose={onClose}
      title={backend?.name ?? backend?.backendId ?? "Backend"}
      description={
        backend
          ? [
              backend.endpoint,
              (() => {
                const n = deployment.inferenceProfiles.filter(
                  (p) => p.backend_id === backend.backendId,
                ).length;
                return `serves ${n} ${n === 1 ? "profile" : "profiles"}`;
              })(),
            ]
              .filter(Boolean)
              .join(" · ")
          : undefined
      }
      page={
        backend
          ? {
              name: "agent",
              agentDid: deployment.agentDid,
              section: "inference",
              item: backend.backendId,
            }
          : undefined
      }
    >
      {backend && (
        <BackendEditor
          key={backend.backendId}
          shell={shell}
          deployment={deployment}
          backend={backend}
          accounts={accounts}
          reload={reload}
          embedded
        />
      )}
    </EditorSheet>
  );
}
